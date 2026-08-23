//! GLYPH-57 acceptance: the engine thread — command/report loop over SPSC
//! with a sink it owns for the life of the thread.
//!
//! Every test drives a real spawned engine through real channels: control
//! sends only `EngineCommand`s and reads only `EngineReport`s, exactly as
//! the future MCP server will. Audio is inspected byte-level out of the
//! WAV the engine wrote.
//!
//! Harness mirrors `seq_voice.rs`: build a throwaway glyph project holding
//! the three sequencer modules plus a test-specific `main.glyph`, build it
//! with the real `glyph` project tool, run it, inspect the output.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::Path;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tempfile::TempDir;

const SEQ_PROTO_SRC: &str = include_str!("../../../examples/sequencer/src/seq_proto.glyph");
const SEQ_VOICE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_voice.glyph");
const SEQ_ENGINE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_engine.glyph");

/// Imports plus the one report-inspection helper every test main needs.
///
/// It exists because a payload-of-payload match loses its binding's type
/// (SEQUENCER_CORE.md §3 constraint #5): the inner match has to be routed
/// through a fn with a typed `EngineReport` parameter.
///
/// It returns everything at once because Glyph is single-owner: passing a
/// report to `rep_kind(rep)` MOVES it, so a second `rep_frame(rep)` on the
/// same value is a use-after-move. One call, one struct, one move.
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

// kind: 1 Position, 2 BlockRendered, 3 Overflow, 4 Stopped
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

/// Builds a temp glyph project with the three sequencer modules plus
/// `main_src`, builds it through the real `glyph` tool, and runs it with
/// the project root as cwd so any WAV lands there.
fn build_and_run(main_src: &str) -> (Option<i32>, TempDir) {
    let temp = TempDir::new().unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return (None, temp);
    }

    let root = temp.path();
    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "seqengine"
version = "0.1.0"

[[bin]]
name = "seqengine"
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

    let exe = root.join("target/debug/seqengine");
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
    assert_eq!(&bytes[8..12], b"WAVE");
    bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// A complete offline session: stream a one-bar pattern in, render exactly
/// `frames` frames, wait for `Stopped`, then shut down and join.
///
/// Shutdown is sent only AFTER `Stopped` arrives, because Shutdown is
/// immediate (ADR §9) — queuing it behind RenderTo would cancel the render.
const RENDER_SESSION: &str = r#"
fn render_session(path: str, frames: u64) -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create(path, 48000, 1)
  let wav_handle: i32 = match made {
    Ok(w) => w.handle,
    Err(_e) => { ret 10 },
  }

  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let c3 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 480, pitch: 60, velocity: 0.9 }))
  match c3 { Sent => {}, Full(_e2) => { ret 24 }, Disconnected(_f) => { ret 25 }, }
  let c4 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 1920, length_ticks: 480, pitch: 67, velocity: 0.8 }))
  match c4 { Sent => {}, Full(_g) => { ret 26 }, Disconnected(_h) => { ret 27 }, }
  let c5 = cmd_tx.try_send(EngineCommand::PatternCommit)
  match c5 { Sent => {}, Full(_i) => { ret 28 }, Disconnected(_j) => { ret 29 }, }
  let c6 = cmd_tx.try_send(EngineCommand::RenderTo(frames))
  match c6 { Sent => {}, Full(_k) => { ret 30 }, Disconnected(_l) => { ret 31 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }

  let started = spawn(engine)
  let handle = match started {
    Ok(h) => h,
    Err(_se) => { ret 41 },
  }

  let mut seen_stopped: bool = false
  let mut spins: i32 = 0
  while seen_stopped == false && spins < 20000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.kind == 4 { seen_stopped = true }
      },
      Empty => {
        let _sl = sleep_ms(1)
        spins = spins + 1
      },
      Disconnected => { seen_stopped = true },
    }
  }
  if seen_stopped == false { ret 60 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 32 }, Disconnected(_n) => {}, }

  ret match handle.join() {
    Ok(code) => code,
    Err(_je) => 40,
  }
}
"#;

/// The headline GLYPH-57 acceptance: an offline engine run driven purely by
/// commands produces a deterministic WAV.
#[test]
fn command_driven_render_is_deterministic() {
    let main_src = format!(
        "{}\n{}",
        RENDER_SESSION,
        r#"
fn main() -> i32 {
  let a = render_session("det1.wav", 96000)
  if a != 0 { ret a }
  let b = render_session("det2.wav", 96000)
  if b != 0 { ret 100 + b }
  ret 0
}
"#
    );

    let (code, temp) = build_and_run(&main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "engine session exited nonzero");

    let bytes1 = fs::read(temp.path().join("det1.wav")).unwrap();
    let bytes2 = fs::read(temp.path().join("det2.wav")).unwrap();
    assert_eq!(
        bytes1, bytes2,
        "two identical command sequences produced different audio"
    );

    // Not just deterministic — deterministically the RIGHT audio.
    let samples = read_wav_samples(&temp.path().join("det1.wav"));
    assert_eq!(samples.len(), 96000, "RenderTo(96000) must render exactly");
    assert!(
        samples[0..12000].iter().any(|&s| s != 0),
        "the note at tick 0 did not sound"
    );
    assert!(
        samples[20000..47000].iter().all(|&s| s == 0),
        "gap between the two notes should be silent"
    );
    assert!(
        samples[48000..60000].iter().any(|&s| s != 0),
        "the note at tick 1920 did not sound"
    );
}

/// `RenderTo(n)` exists so offline renders are frame-exact rather than a
/// race against Shutdown (ADR §6). Every target must land on the nose.
#[test]
fn render_to_lands_on_the_exact_frame() {
    for target in [256u64, 1000, 48000] {
        let main_src = format!(
            "{}\nfn main() -> i32 {{ ret render_session(\"exact.wav\", {}) }}",
            RENDER_SESSION, target
        );
        let (code, temp) = build_and_run(&main_src);
        let Some(code) = code else { return };
        assert_eq!(code, 0, "session for target {} exited nonzero", target);

        let samples = read_wav_samples(&temp.path().join("exact.wav"));
        assert_eq!(
            samples.len() as u64,
            target,
            "RenderTo({}) produced {} frames",
            target,
            samples.len()
        );
    }
}

/// A target at or behind the playhead is a no-op render that still reports
/// Stopped, so control never waits forever for a render that cannot happen.
#[test]
fn render_to_in_the_past_is_a_no_op_that_still_reports_stopped() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("noop.wav", 48000, 1)
  let wav_handle: i32 = match made {
    Ok(w) => w.handle,
    Err(_e) => { ret 10 },
  }

  let c = cmd_tx.try_send(EngineCommand::RenderTo(0))
  match c { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut stopped_at: u64 = 999
  let mut seen: bool = false
  let mut spins: i32 = 0
  while seen == false && spins < 5000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.kind == 4 {
          seen = true
          stopped_at = info.frame
        }
      },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen = true },
    }
  }
  if seen == false { ret 60 }
  if stopped_at != 0 { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 32 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "no-op render session exited nonzero");
    let samples = read_wav_samples(&temp.path().join("noop.wav"));
    assert!(samples.is_empty(), "a no-op render must write no audio");
}

/// The report stream carries monotonically increasing positions, and the
/// engine reports itself as playing while it is.
#[test]
fn position_reports_are_monotonic() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("mono.wav", 48000, 1)
  let wav_handle: i32 = match made {
    Ok(w) => w.handle,
    Err(_e) => { ret 10 },
  }

  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 960, pitch: 60, velocity: 0.9 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let c3 = cmd_tx.try_send(EngineCommand::PatternCommit)
  match c3 { Sent => {}, Full(_e2) => { ret 24 }, Disconnected(_f) => { ret 25 }, }
  let c4 = cmd_tx.try_send(EngineCommand::RenderTo(24000))
  match c4 { Sent => {}, Full(_g) => { ret 26 }, Disconnected(_h) => { ret 27 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut last_frame: u64 = 0
  let mut positions: i32 = 0
  let mut playing_seen: i32 = 0
  let mut seen_stopped: bool = false
  let mut spins: i32 = 0
  while seen_stopped == false && spins < 20000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.kind == 4 { seen_stopped = true }
        if info.kind == 1 {
          // strictly increasing: every block advances the frame counter
          if info.frame <= last_frame { ret 70 }
          last_frame = info.frame
          positions = positions + 1
          if info.playing == 1 { playing_seen = playing_seen + 1 }
        }
      },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen_stopped = true },
    }
  }
  if seen_stopped == false { ret 60 }
  // 24000 frames / 256 per block => 94 full blocks + one short block
  if positions < 90 { ret 71 }
  if playing_seen < 90 { ret 72 }
  if last_frame != 24000 { ret 73 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 32 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "position report stream failed its in-program checks (see exit-code map)"
    );
}

/// ADR §9 step 4: if control drops its endpoints without sending Shutdown,
/// the engine's next channel op yields Disconnected and it takes the same
/// finalize path. The engine can never hang on a dead peer, because no
/// channel operation blocks.
#[test]
fn dropped_command_sender_makes_the_engine_self_stop() {
    let main_src = r#"
// Takes ownership of the Sender and drops it on return.
fn queue_then_drop(tx: Sender<EngineCommand>) -> i32 {
  let c = tx.try_send(EngineCommand::Rewind)
  ret match c {
    Sent => 0,
    Full(_a) => 1,
    Disconnected(_b) => 2,
  }
}

fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("disc.wav", 48000, 1)
  let wav_handle: i32 = match made {
    Ok(w) => w.handle,
    Err(_e) => { ret 10 },
  }

  let qrc = queue_then_drop(cmd_tx)
  if qrc != 0 { ret 11 }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  ret match started {
    Ok(h) => match h.join() { Ok(code) => code, Err(_je) => 40, },
    Err(_se) => 41,
  }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "engine did not self-stop cleanly on Disconnected");

    // The sink was still finalized: a valid, empty WAV rather than a
    // truncated file with an unpatched RIFF header.
    let bytes = fs::read(temp.path().join("disc.wav")).unwrap();
    assert_eq!(bytes.len(), 44, "expected a header-only WAV");
    assert_eq!(&bytes[0..4], b"RIFF");
}

/// Live notes sound regardless of transport state — auditioning while
/// stopped is intended MCP behaviour (ADR §4, NoteOn).
#[test]
fn live_note_sounds_while_transport_is_stopped() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("live.wav", 48000, 1)
  let wav_handle: i32 = match made {
    Ok(w) => w.handle,
    Err(_e) => { ret 10 },
  }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  // Never Play: the transport stays stopped for the whole session.
  let n1 = cmd_tx.try_send(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: 69, velocity: 1.0 }))
  match n1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let _s1 = sleep_ms(60)
  let n2 = cmd_tx.try_send(EngineCommand::NoteOff(NoteCmd { track: 0, pitch: 69, velocity: 0.0 }))
  match n2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let _s2 = sleep_ms(120)

  // No Position report may ever claim the transport was playing.
  let mut playing_seen: i32 = 0
  let mut drained: bool = false
  while drained == false {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.playing == 1 { playing_seen = playing_seen + 1 }
      },
      Empty => { drained = true },
      Disconnected => { drained = true },
    }
  }
  if playing_seen != 0 { ret 70 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 32 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "live-note session exited nonzero");

    let samples = read_wav_samples(&temp.path().join("live.wav"));
    assert!(
        !samples.is_empty(),
        "engine rendered nothing for a live note"
    );
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(
        peak > 1000,
        "live note while stopped was silent (peak {})",
        peak
    );
}
