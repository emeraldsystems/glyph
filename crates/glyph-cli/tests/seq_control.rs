//! GLYPH-59 acceptance: `SequencerController` — the control-side API
//! shaped for MCP.
//!
//! The headline test drives a complete session (load / render / status /
//! shutdown) through `apply_json_command` strings and nothing else, which
//! is exactly the surface the MCP server will expose. The rest exercise
//! the dispatch surface directly: it is a pure function over
//! `ControllerState`, so most of it needs no engine thread at all.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tempfile::TempDir;

const SEQ_PROTO_SRC: &str = include_str!("../../../examples/sequencer/src/seq_proto.glyph");
const SEQ_VOICE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_voice.glyph");
const SEQ_ENGINE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_engine.glyph");
const SEQ_MODEL_SRC: &str = include_str!("../../../examples/sequencer/src/seq_model.glyph");
const SEQ_CONTROL_SRC: &str = include_str!("../../../examples/sequencer/src/seq_control.glyph");

const PRELUDE: &str = r#"
import std
from seq_control import ControllerState, controller_new, controller_started, apply_json_command, send_commands, note_report, status_json, state_running, state_stopping
from seq_proto import EngineCommand, EngineReport, sink_wav
from seq_engine import engine_run
from std/audio import WavWriter, wav_create
from std/enums import Result
from std/string import string_index_of
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
from std/thread import JoinHandle, ThreadError, spawn
from std/time import sleep_ms
from std/vec import Vec

// These take `str`, not `String`: passing an owned String to a helper MOVES
// it, so a reply could only ever be asked one question. Call sites coerce
// once with `let rs: str = reply` and then inspect freely.
fn reply_ok(s: str) -> bool {
  ret string_index_of(s, "\"ok\":true") > 0
}

fn reply_has(s: str, needle: str) -> bool {
  ret string_index_of(s, needle) > 0
}

// Discriminant of a planned command, so tests can assert on the exact wire
// sequence a verb expands to.
fn cmd_tag(c: EngineCommand) -> i32 {
  ret match c {
    Play => 1,
    Stop => 2,
    Rewind => 3,
    SetTempo(_b) => 4,
    RenderTo(_f) => 5,
    PatternBegin(_h) => 6,
    PatternNote(_n) => 7,
    PatternTrack(_t) => 8,
    PatternCommit => 9,
    NoteOn(_o) => 10,
    NoteOff(_f2) => 11,
    SetTrackGain(_g) => 12,
    Shutdown => 13,
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
name = "seqcontrol"
version = "0.1.0"

[[bin]]
name = "seqcontrol"
path = "src/main.glyph"
"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/seq_proto.glyph"), SEQ_PROTO_SRC).unwrap();
    fs::write(root.join("src/seq_voice.glyph"), SEQ_VOICE_SRC).unwrap();
    fs::write(root.join("src/seq_engine.glyph"), SEQ_ENGINE_SRC).unwrap();
    fs::write(root.join("src/seq_model.glyph"), SEQ_MODEL_SRC).unwrap();
    fs::write(root.join("src/seq_control.glyph"), SEQ_CONTROL_SRC).unwrap();
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

    let exe = root.join("target/debug/seqcontrol");
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

/// The GLYPH-59 headline: a complete session — load a song, render a bar,
/// read status, shut down — driven through `apply_json_command` strings
/// and nothing else.
#[test]
fn full_session_runs_through_json_commands_only() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("session.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut st = controller_new()
  let _s = controller_started(&mut st)
  let mut backlog: Vec<EngineCommand> = Vec::new()

  // load_song: one JSON string becomes the whole streamed pattern.
  let mut plan1: Vec<EngineCommand> = Vec::new()
  let song = "{\"cmd\":\"load_song\",\"song\":{\"bpm\":120,\"ppq\":960,\"length_ticks\":3840,\"tracks\":[{\"track\":0,\"waveform\":0,\"gain\":0.8,\"attack_ms\":5,\"decay_ms\":50,\"sustain\":0.7,\"release_ms\":80}],\"notes\":[{\"track\":0,\"tick\":0,\"length_ticks\":480,\"pitch\":60,\"velocity\":0.9},{\"track\":0,\"tick\":1920,\"length_ticks\":480,\"pitch\":67,\"velocity\":0.8}]}}"
  let r1 = apply_json_command(song, &mut st, &mut plan1)
  let rs1: str = r1
  if reply_ok(rs1) == false { ret 50 }
  let mut next1: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan1, &mut next1)
  backlog = next1
  if backlog.len() != 0 { ret 51 }

  // render exactly one bar
  let mut plan2: Vec<EngineCommand> = Vec::new()
  let r2 = apply_json_command("{\"cmd\":\"render_to\",\"frame\":96000}", &mut st, &mut plan2)
  let rs2: str = r2
  if reply_ok(rs2) == false { ret 52 }
  let mut next2: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan2, &mut next2)
  backlog = next2

  // fold telemetry into the control-side mirror until the render lands
  let mut spins: i32 = 0
  while st.stopped_seen == 0 && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let _k = note_report(&mut st, rep) },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { spins = 60000 },
    }
  }
  if st.stopped_seen == 0 { ret 60 }
  if st.frame != 96000 { ret 61 }

  // status is answered from the mirror
  let stat = status_json(&st)
  let rstat: str = stat
  if reply_ok(rstat) == false { ret 62 }
  if reply_has(rstat, "\"frame\":96000") == false { ret 63 }

  // shutdown through the same surface
  let mut plan3: Vec<EngineCommand> = Vec::new()
  let r3 = apply_json_command("{\"cmd\":\"shutdown\"}", &mut st, &mut plan3)
  let rs3: str = r3
  if reply_ok(rs3) == false { ret 64 }
  if st.state != state_stopping() { ret 65 }
  let mut next3: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan3, &mut next3)

  // and the controller refuses transport commands once it is Stopping
  let mut plan4: Vec<EngineCommand> = Vec::new()
  let r4 = apply_json_command("{\"cmd\":\"play\"}", &mut st, &mut plan4)
  let rs4: str = r4
  if reply_ok(rs4) { ret 66 }
  if plan4.len() != 0 { ret 67 }

  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "JSON-only session failed its in-program checks (see exit-code map)"
    );

    let bytes = fs::read(temp.path().join("session.wav")).unwrap();
    assert_eq!(
        bytes.len(),
        44 + 96_000 * 2,
        "the JSON-driven render did not produce exactly one bar"
    );
}

/// Malformed and invalid commands produce a structured error reply and
/// plan NOTHING, so a bad request provably cannot reach the engine.
#[test]
fn bad_commands_are_refused_without_planning_anything() {
    let main_src = r#"
// Every one of these must be rejected AND plan zero commands.
fn reject(cmd: str, st: &mut ControllerState) -> i32 {
  let mut plan: Vec<EngineCommand> = Vec::new()
  let reply = apply_json_command(cmd, st, &mut plan)
  let rs: str = reply
  if reply_ok(rs) { ret 1 }
  if plan.len() != 0 { ret 2 }
  ret 0
}

fn main() -> i32 {
  let mut st = controller_new()
  let _s = controller_started(&mut st)

  // malformed / wrong-shaped JSON
  if reject("{not json", &mut st) != 0 { ret 20 }
  if reject("", &mut st) != 0 { ret 21 }
  if reject("[1,2,3]", &mut st) != 0 { ret 22 }
  if reject("\"just a string\"", &mut st) != 0 { ret 23 }
  if reject("{\"nocmd\":1}", &mut st) != 0 { ret 24 }
  if reject("{\"cmd\":7}", &mut st) != 0 { ret 25 }
  if reject("{\"cmd\":\"fly\"}", &mut st) != 0 { ret 26 }

  // missing operands
  if reject("{\"cmd\":\"set_tempo\"}", &mut st) != 0 { ret 30 }
  if reject("{\"cmd\":\"note_on\",\"track\":0,\"pitch\":60}", &mut st) != 0 { ret 31 }
  if reject("{\"cmd\":\"render_to\"}", &mut st) != 0 { ret 32 }

  // wrong-typed operands
  if reject("{\"cmd\":\"set_tempo\",\"bpm\":\"fast\"}", &mut st) != 0 { ret 40 }

  // out-of-range operands
  if reject("{\"cmd\":\"set_tempo\",\"bpm\":0}", &mut st) != 0 { ret 50 }
  if reject("{\"cmd\":\"set_tempo\",\"bpm\":5000}", &mut st) != 0 { ret 51 }
  if reject("{\"cmd\":\"note_on\",\"track\":0,\"pitch\":999,\"velocity\":0.5}", &mut st) != 0 { ret 52 }
  if reject("{\"cmd\":\"note_on\",\"track\":0,\"pitch\":60,\"velocity\":9}", &mut st) != 0 { ret 53 }
  if reject("{\"cmd\":\"note_on\",\"track\":99,\"pitch\":60,\"velocity\":0.5}", &mut st) != 0 { ret 54 }
  if reject("{\"cmd\":\"set_track_gain\",\"track\":0,\"gain\":99}", &mut st) != 0 { ret 55 }

  // a song that fails to parse must not partially load
  if reject("{\"cmd\":\"load_song\",\"song\":{\"bpm\":120}}", &mut st) != 0 { ret 60 }
  if reject("{\"cmd\":\"load_song\"}", &mut st) != 0 { ret 61 }
  if st.song_notes != 0 { ret 62 }

  // and after all that abuse the controller is still Running and healthy
  if st.state != state_running() { ret 70 }
  let mut good: Vec<EngineCommand> = Vec::new()
  let r = apply_json_command("{\"cmd\":\"play\"}", &mut st, &mut good)
  let rsg: str = r
  if reply_ok(rsg) == false { ret 71 }
  if good.len() != 1 { ret 72 }
  if cmd_tag(good[0]) != 1 { ret 73 }

  ret 0
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "a bad command was accepted or planned work (see exit-code map)"
    );
}

/// One `load_song` expands into the exact streamed wire sequence: tempo,
/// PatternBegin, one PatternTrack per track, one PatternNote per note,
/// PatternCommit — because a pattern cannot cross the channel as one value.
#[test]
fn load_song_expands_to_the_streamed_wire_form() {
    let main_src = r#"
fn main() -> i32 {
  let mut st = controller_new()
  let _s = controller_started(&mut st)
  let mut plan: Vec<EngineCommand> = Vec::new()

  let song = "{\"cmd\":\"load_song\",\"song\":{\"bpm\":140,\"ppq\":960,\"length_ticks\":7680,\"tracks\":[{\"track\":0,\"waveform\":0,\"gain\":0.8,\"attack_ms\":5,\"decay_ms\":50,\"sustain\":0.7,\"release_ms\":80},{\"track\":1,\"waveform\":2,\"gain\":0.5,\"attack_ms\":1,\"decay_ms\":20,\"sustain\":0.4,\"release_ms\":40}],\"notes\":[{\"track\":0,\"tick\":0,\"length_ticks\":480,\"pitch\":60,\"velocity\":0.9},{\"track\":1,\"tick\":960,\"length_ticks\":240,\"pitch\":48,\"velocity\":0.7},{\"track\":0,\"tick\":1920,\"length_ticks\":480,\"pitch\":67,\"velocity\":0.8}]}}"
  let reply = apply_json_command(song, &mut st, &mut plan)
  let rs: str = reply
  if reply_ok(rs) == false { ret 10 }
  if reply_has(rs, "\"tracks\":2") == false { ret 11 }
  if reply_has(rs, "\"notes\":3") == false { ret 12 }

  // SetTempo, PatternBegin, 2 x PatternTrack, 3 x PatternNote, PatternCommit
  if plan.len() != 8 { ret 20 + plan.len() as i32 }
  if cmd_tag(plan[0]) != 4 { ret 30 }
  if cmd_tag(plan[1]) != 6 { ret 31 }
  if cmd_tag(plan[2]) != 8 { ret 32 }
  if cmd_tag(plan[3]) != 8 { ret 33 }
  if cmd_tag(plan[4]) != 7 { ret 34 }
  if cmd_tag(plan[5]) != 7 { ret 35 }
  if cmd_tag(plan[6]) != 7 { ret 36 }
  if cmd_tag(plan[7]) != 9 { ret 37 }

  if st.bpm != 140.0 { ret 40 }
  if st.song_length_ticks != 7680 { ret 41 }
  ret 0
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "load_song did not expand as expected");
}

/// Commands are lossless under backpressure (ADR §4): when the channel
/// fills, `try_send` hands the command back, and it plus everything after
/// it is deferred — so nothing is dropped and ORDER is preserved.
#[test]
fn backpressure_defers_commands_losslessly_and_in_order() {
    let main_src = r#"
fn main() -> i32 {
  // No engine: nothing drains the channel, so it must fill.
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)

  let mut st = controller_new()
  let _s = controller_started(&mut st)

  // Plan 100 note_ons with ascending pitches, so order is checkable.
  let mut plan: Vec<EngineCommand> = Vec::new()
  let mut i: i32 = 0
  while i < 100 {
    let pitch: u32 = 20 + i as u32
    plan.push(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: pitch, velocity: 0.5 }))
    i = i + 1
  }

  let mut backlog: Vec<EngineCommand> = Vec::new()
  let mut next: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan, &mut next)
  backlog = next
  // The channel holds 64, so some must have been deferred rather than lost.
  if backlog.len() == 0 { ret 10 }
  if backlog.len() >= 100 { ret 11 }

  // Drain what made it through, checking pitches are consecutive from 20.
  let mut expect: u32 = 20
  let mut received: i32 = 0
  let mut draining: bool = true
  while draining {
    let got = cmd_rx.try_recv()
    match got {
      Value(c) => {
        if note_pitch(c) != expect { ret 20 }
        expect = expect + 1
        received = received + 1
      },
      Empty => { draining = false },
      Disconnected => { draining = false },
    }
  }
  if received == 0 { ret 21 }

  // Now retry the backlog; with the channel drained it must all go.
  let mut empty_fresh: Vec<EngineCommand> = Vec::new()
  let mut next2: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &empty_fresh, &mut next2)
  if next2.len() != 0 { ret 30 }

  let mut draining2: bool = true
  while draining2 {
    let got2 = cmd_rx.try_recv()
    match got2 {
      Value(c2) => {
        if note_pitch(c2) != expect { ret 31 }
        expect = expect + 1
        received = received + 1
      },
      Empty => { draining2 = false },
      Disconnected => { draining2 = false },
    }
  }

  // Every command arrived exactly once, in the order it was planned.
  if received != 100 { ret 40 }
  if expect != 120 { ret 41 }
  ret 0
}

fn note_pitch(c: EngineCommand) -> u32 {
  ret match c {
    NoteOn(n) => n.pitch,
    _ => 9999,
  }
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "backpressure lost or reordered commands (see exit-code map)"
    );
}

/// Commands issued before the engine is started are refused by the
/// controller without touching the channel (ADR §10).
#[test]
fn commands_before_start_are_refused() {
    let main_src = r#"
fn main() -> i32 {
  let mut st = controller_new()   // Idle: never started
  let mut plan: Vec<EngineCommand> = Vec::new()

  let r = apply_json_command("{\"cmd\":\"play\"}", &mut st, &mut plan)
  let rs: str = r
  if reply_ok(rs) { ret 10 }
  if plan.len() != 0 { ret 11 }

  // status still works while Idle — it only reads the mirror.
  let s = status_json(&st)
  let rss: str = s
  if reply_ok(rss) == false { ret 12 }

  let r2 = apply_json_command("{\"cmd\":\"status\"}", &mut st, &mut plan)
  let rs2: str = r2
  if reply_ok(rs2) == false { ret 13 }
  if plan.len() != 0 { ret 14 }

  ret 0
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "controller accepted a command while Idle");
}
