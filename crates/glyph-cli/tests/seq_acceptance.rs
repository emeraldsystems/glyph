//! GLYPH-61 acceptance + stress: the release gate for the GLYPH-53
//! sequencer epic.
//!
//! Determinism, command floods, transport chaos, a long-run heap check and
//! the live-note latency bound. The live-output smoke test is env-gated
//! (`GLYPH_AUDIO_LIVE_TEST=1`) because it needs a real audio device.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tempfile::TempDir;

const SEQ_PROTO_SRC: &str = include_str!("../../../examples/sequencer/src/seq_proto.glyph");
const SEQ_VOICE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_voice.glyph");
const SEQ_ENGINE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_engine.glyph");
const SEQ_MODEL_SRC: &str = include_str!("../../../examples/sequencer/src/seq_model.glyph");
const SEQ_CONTROL_SRC: &str = include_str!("../../../examples/sequencer/src/seq_control.glyph");

/// The canonical fixture: 2 bars, 3 tracks, 14 notes. Shared with the
/// `examples/sequencer` demo so the demo and the release gate can never
/// drift apart.
const CANONICAL_SONG: &str = include_str!("../../../examples/sequencer/song.json");

/// Content hash of the canonical 4-bar render.
///
/// If a deliberate DSP or scheduler change lands, this MUST be updated in
/// the same commit — that is the point of pinning it.
const EXPECTED_FNV1A: u64 = 0x9baf_8ecd_b86d_b9b7;

const PRELUDE: &str = r#"
import std
from seq_control import ControllerState, controller_new, controller_started, apply_json_command, send_commands, note_report, status_json
from seq_proto import EngineCommand, EngineReport, PatternHeader, PatternNoteMsg, TrackParamsMsg, NoteCmd, GainCmd, PositionReport, BlockReport, sink_wav, sink_live
from seq_engine import engine_run
from std/audio import WavWriter, wav_create
from std/enums import Result
from std/string import string_index_of
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
from std/thread import JoinHandle, ThreadError, spawn
from std/time import sleep_ms, now_monotonic, Instant
from std/vec import Vec

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

fn reply_ok(s: str) -> bool {
  ret string_index_of(s, "\"ok\":true") > 0
}
"#;

/// Escapes a JSON document so it can be embedded as a Glyph string literal,
/// collapsing it to one line first (Glyph literals are single-line).
fn as_glyph_literal(json: &str) -> String {
    let mut compact = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in json.chars() {
        if in_string {
            compact.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                compact.push(c);
            }
            ' ' | '\n' | '\r' | '\t' => {}
            _ => compact.push(c),
        }
    }
    compact.replace('\\', "\\\\").replace('"', "\\\"")
}

fn build_project(main_src: &str) -> Option<(TempDir, PathBuf)> {
    let temp = TempDir::new().unwrap();
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return None;
    }

    let root = temp.path();
    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "seqacc"
version = "0.1.0"

[[bin]]
name = "seqacc"
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

    let exe = root.join("target/debug/seqacc");
    assert!(exe.exists(), "expected built binary at {}", exe.display());
    Some((temp, exe))
}

fn build_and_run(main_src: &str) -> (Option<i32>, TempDir) {
    match build_project(main_src) {
        None => (None, TempDir::new().unwrap()),
        Some((temp, exe)) => {
            let run = Command::new(&exe).current_dir(temp.path()).output().unwrap();
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
    }
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

/// FNV-1a. A dependency-free content hash: enough to notice that the DSP
/// output changed, which is all this gate needs.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Peak resident set size of a child process, in bytes.
///
/// macOS `/usr/bin/time -l` reports it on stderr; elsewhere we skip the
/// measurement rather than guess.
#[cfg(target_os = "macos")]
fn run_with_peak_rss(exe: &Path, cwd: &Path) -> (Option<i32>, Option<u64>) {
    let out = Command::new("/usr/bin/time")
        .arg("-l")
        .arg(exe)
        .current_dir(cwd)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let rss = stderr.lines().find_map(|line| {
        let line = line.trim();
        line.strip_suffix("maximum resident set size")
            .and_then(|n| n.trim().parse::<u64>().ok())
    });
    (out.status.code(), rss)
}

#[cfg(not(target_os = "macos"))]
fn run_with_peak_rss(exe: &Path, cwd: &Path) -> (Option<i32>, Option<u64>) {
    let out = Command::new(exe).current_dir(cwd).output().unwrap();
    (out.status.code(), None)
}

/// Renders the canonical song offline; `path` and `frames` are the only
/// knobs. Used by several tests below.
fn canonical_render_program(path: &str, frames: u64) -> String {
    format!(
        r#"
fn render_canonical(path: str, frames: u64) -> i32 {{
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create(path, 48000, 1)
  let wav_handle: i32 = match made {{ Ok(w) => w.handle, Err(_e) => {{ ret 10 }}, }}

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {{
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }}
  let started = spawn(engine)
  let handle = match started {{ Ok(h) => h, Err(_se) => {{ ret 41 }}, }}

  let mut st = controller_new()
  let _s = controller_started(&mut st)
  let mut backlog: Vec<EngineCommand> = Vec::new()

  let mut plan: Vec<EngineCommand> = Vec::new()
  let song = "{{\"cmd\":\"load_song\",\"song\":{song}}}"
  let reply = apply_json_command(song, &mut st, &mut plan)
  let rs: str = reply
  if reply_ok(rs) == false {{ ret 50 }}
  let mut next: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan, &mut next)
  backlog = next
  if backlog.len() != 0 {{ ret 51 }}

  let mut plan2: Vec<EngineCommand> = Vec::new()
  plan2.push(EngineCommand::RenderTo(frames))
  let mut next2: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &plan2, &mut next2)
  backlog = next2

  let mut spins: i32 = 0
  while st.stopped_seen == 0 && spins < 600000 {{
    let got = rep_rx.try_recv()
    match got {{
      Value(rep) => {{ let _k = note_report(&mut st, rep) }},
      Empty => {{ let _sl = sleep_ms(1) spins = spins + 1 }},
      Disconnected => {{ spins = 600000 }},
    }}
  }}
  if st.stopped_seen == 0 {{ ret 60 }}
  if st.frame != frames {{ ret 61 }}

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs {{ Sent => {{}}, Full(_m) => {{ ret 62 }}, Disconnected(_n) => {{}}, }}
  ret match handle.join() {{ Ok(code) => code, Err(_je) => 40, }}
}}

fn main() -> i32 {{
  ret render_canonical("{path}", {frames})
}}
"#,
        song = as_glyph_literal(CANONICAL_SONG),
        path = path,
        frames = frames
    )
}

/// Determinism gate: the canonical song renders to byte-identical audio
/// every time, and its content hash is pinned.
///
/// If a deliberate DSP or scheduler change lands, this hash MUST be
/// updated in the same commit — that is the point of pinning it.
#[test]
fn canonical_song_render_is_byte_stable() {
    let mut program = canonical_render_program("canon1.wav", 384_000);
    program = program.replace(
        "fn main() -> i32 {\n  ret render_canonical(\"canon1.wav\", 384000)\n}",
        "fn main() -> i32 {\n  let a = render_canonical(\"canon1.wav\", 384000)\n  if a != 0 { ret a }\n  let b = render_canonical(\"canon2.wav\", 384000)\n  if b != 0 { ret 100 + b }\n  ret 0\n}",
    );

    let (code, temp) = build_and_run(&program);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "canonical render exited nonzero");

    let a = fs::read(temp.path().join("canon1.wav")).unwrap();
    let b = fs::read(temp.path().join("canon2.wav")).unwrap();
    assert_eq!(a, b, "two canonical renders differed");
    assert_eq!(
        a.len(),
        44 + 384_000 * 2,
        "canonical render is not 4 bars long"
    );

    // Musical sanity, so a hash update can never quietly bless silence.
    let samples = read_wav_samples(&temp.path().join("canon1.wav"));
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(peak > 8000, "canonical render is too quiet (peak {})", peak);
    assert!(
        samples[0..1000].iter().any(|&s| s != 0),
        "canonical render does not start on a downbeat"
    );

    let hash = fnv1a64(&a);
    assert_eq!(
        hash, EXPECTED_FNV1A,
        "canonical render hash changed: got {:#018x}, expected {:#018x}. \
         If this was a deliberate DSP/scheduler change, update EXPECTED_FNV1A.",
        hash, EXPECTED_FNV1A
    );
}

/// Command flood: 10k commands as fast as `try_send` allows. The channel
/// must fill (proving backpressure was exercised), nothing may be lost, and
/// the engine must still render correctly afterwards.
#[test]
fn command_flood_is_lossless_and_the_engine_survives() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("flood.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut backlog: Vec<EngineCommand> = Vec::new()
  let mut deferred_at_least_once: bool = false
  let mut sent: i32 = 0

  // 10000 commands that create no voices, so the flood stresses the drain
  // path without turning into an unbounded render.
  let mut i: i32 = 0
  while i < 10000 {
    let mut fresh: Vec<EngineCommand> = Vec::new()
    let g: f64 = 0.5 + (i % 3) as f64 * 0.1
    fresh.push(EngineCommand::SetTrackGain(GainCmd { track: (i % 4) as u32, gain: g }))
    let mut next: Vec<EngineCommand> = Vec::new()
    cmd_tx = send_commands(cmd_tx, &backlog, &fresh, &mut next)
    backlog = next
    if backlog.len() > 0 { deferred_at_least_once = true }
    sent = sent + 1
    i = i + 1
  }

  // Drain the remaining backlog; it must all get through eventually.
  let mut guard: i32 = 0
  while backlog.len() > 0 && guard < 100000 {
    let mut empty: Vec<EngineCommand> = Vec::new()
    let mut next2: Vec<EngineCommand> = Vec::new()
    cmd_tx = send_commands(cmd_tx, &backlog, &empty, &mut next2)
    backlog = next2
    if backlog.len() > 0 { let _sl = sleep_ms(1) }
    guard = guard + 1
  }
  if backlog.len() != 0 { ret 50 }
  if sent != 10000 { ret 51 }
  // A 64-deep channel cannot swallow 10k commands without ever filling.
  if deferred_at_least_once == false { ret 52 }

  // The engine is still healthy: render an exact bar through it.
  let mut fresh3: Vec<EngineCommand> = Vec::new()
  fresh3.push(EngineCommand::RenderTo(48000))
  let mut next3: Vec<EngineCommand> = Vec::new()
  cmd_tx = send_commands(cmd_tx, &backlog, &fresh3, &mut next3)
  backlog = next3

  let mut st = controller_new()
  let _s = controller_started(&mut st)
  let mut spins: i32 = 0
  while st.stopped_seen == 0 && spins < 120000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let _k = note_report(&mut st, rep) },
      Empty => { let _sl2 = sleep_ms(1) spins = spins + 1 },
      Disconnected => { spins = 120000 },
    }
  }
  if st.stopped_seen == 0 { ret 60 }
  if st.frame != 48000 { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 62 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "command flood failed its in-program checks (see exit-code map)"
    );

    let samples = read_wav_samples(&temp.path().join("flood.wav"));
    assert_eq!(
        samples.len(),
        48_000,
        "engine did not render an exact bar after the flood"
    );
}

/// Transport chaos: play / stop / rewind / tempo / pattern-swap interleaved
/// across hundreds of render segments. The absolute frame counter must stay
/// strictly monotonic throughout and the engine must survive.
#[test]
fn transport_chaos_keeps_positions_monotonic() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("chaos.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut backlog: Vec<EngineCommand> = Vec::new()
  let mut last_frame: u64 = 0
  let mut target: u64 = 0
  let mut stops_seen: i32 = 0

  let mut i: i32 = 0
  while i < 200 {
    let mut fresh: Vec<EngineCommand> = Vec::new()

    // A fixed, overflow-free stir rather than a PRNG: reproducible chaos.
    let pick: i32 = (i * 7 + 3) % 6
    if pick == 0 { fresh.push(EngineCommand::Play) }
    if pick == 1 { fresh.push(EngineCommand::Stop) }
    if pick == 2 { fresh.push(EngineCommand::SetTempo(90.0 + (i % 5) as f64 * 30.0)) }
    if pick == 3 { fresh.push(EngineCommand::Rewind) }
    if pick == 4 {
      fresh.push(EngineCommand::PatternBegin(PatternHeader { length_ticks: 1920, track_count: 1 }))
      fresh.push(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: (i % 4) as u32, gain: 0.6, attack_ms: 3.0, decay_ms: 30.0, sustain: 0.6, release_ms: 50.0 }))
      fresh.push(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 240, pitch: 55 + (i % 12) as u32, velocity: 0.8 }))
      fresh.push(EngineCommand::PatternCommit)
    }
    if pick == 5 {
      fresh.push(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: 60 + (i % 7) as u32, velocity: 0.7 }))
      fresh.push(EngineCommand::NoteOff(NoteCmd { track: 0, pitch: 60 + (i % 7) as u32, velocity: 0.0 }))
    }

    // Bound every segment so the offline render cannot run away.
    target = target + 2048
    fresh.push(EngineCommand::RenderTo(target))

    let mut next: Vec<EngineCommand> = Vec::new()
    cmd_tx = send_commands(cmd_tx, &backlog, &fresh, &mut next)
    backlog = next

    // Drain until this segment reports Stopped, checking monotonicity.
    let mut segment_done: bool = false
    let mut spins: i32 = 0
    while segment_done == false && spins < 60000 {
      let got = rep_rx.try_recv()
      match got {
        Value(rep) => {
          let info = rep_info(rep)
          if info.kind == 1 {
            if info.frame <= last_frame { ret 70 }
            last_frame = info.frame
          }
          if info.kind == 4 {
            segment_done = true
            stops_seen = stops_seen + 1
            if info.frame != target { ret 71 }
          }
        },
        Empty => {
          // Retry any backlog while waiting, or a full channel deadlocks.
          if backlog.len() > 0 {
            let mut empty2: Vec<EngineCommand> = Vec::new()
            let mut next2: Vec<EngineCommand> = Vec::new()
            cmd_tx = send_commands(cmd_tx, &backlog, &empty2, &mut next2)
            backlog = next2
          }
          let _sl = sleep_ms(1)
          spins = spins + 1
        },
        Disconnected => { ret 72 },
      }
    }
    if segment_done == false { ret 73 }
    i = i + 1
  }

  if stops_seen != 200 { ret 80 }
  if last_frame != target { ret 81 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 82 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "transport chaos failed its in-program checks (see exit-code map)"
    );

    let samples = read_wav_samples(&temp.path().join("chaos.wav"));
    assert_eq!(
        samples.len(),
        200 * 2048,
        "chaos run did not render the exact requested frame count"
    );
}

/// Long run: a 60 s offline render must complete exactly and must not grow
/// the heap relative to a 6 s render. Per-block allocation that is never
/// freed would show up as a peak-RSS blowup (the GLYPH-19 lesson).
#[test]
fn long_run_render_is_exact_and_heap_stable() {
    let short = canonical_render_program("short.wav", 288_000); // 6 s
    let long = canonical_render_program("long.wav", 2_880_000); // 60 s

    let Some((short_dir, short_exe)) = build_project(&short) else {
        return;
    };
    let Some((long_dir, long_exe)) = build_project(&long) else {
        return;
    };

    let (short_code, short_rss) = run_with_peak_rss(&short_exe, short_dir.path());
    let (long_code, long_rss) = run_with_peak_rss(&long_exe, long_dir.path());

    assert_eq!(short_code, Some(0), "6 s render exited nonzero");
    assert_eq!(long_code, Some(0), "60 s render exited nonzero");

    let short_len = fs::read(short_dir.path().join("short.wav")).unwrap().len();
    let long_len = fs::read(long_dir.path().join("long.wav")).unwrap().len();
    assert_eq!(short_len, 44 + 288_000 * 2, "6 s render has the wrong length");
    assert_eq!(
        long_len,
        44 + 2_880_000 * 2,
        "60 s render has the wrong length"
    );

    // Only assert on memory where we can actually measure it.
    if let (Some(s), Some(l)) = (short_rss, long_rss) {
        // A 10x longer render renders 10x the blocks. If each block leaked,
        // peak RSS would scale with it; steady-state use must not.
        let budget = s * 3 + 32 * 1024 * 1024;
        assert!(
            l <= budget,
            "60 s render peak RSS {} B vastly exceeds the 6 s render's {} B \
             (budget {} B) — the render loop is probably leaking per block",
            l,
            s,
            budget
        );
    }
}

/// Live NoteOn is block-quantized: a note commanded while the engine is
/// between blocks sounds in the very next block it renders (ADR §6), which
/// is the < 2 blocks latency bound.
///
/// This is asserted structurally rather than by wall clock, because an
/// offline engine renders far faster than the control thread can observe
/// it — a control-side stopwatch would measure the test harness, not the
/// engine. The wall-clock measurement belongs to the live smoke test below.
#[test]
fn live_note_lands_in_the_next_rendered_block() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("latency.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  // NoteOn and a one-block render arrive in the same drain, so the note is
  // applied before that block is rendered.
  let n1 = cmd_tx.try_send(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: 69, velocity: 1.0 }))
  match n1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let r1 = cmd_tx.try_send(EngineCommand::RenderTo(256))
  match r1 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }

  let mut st = controller_new()
  let _s = controller_started(&mut st)
  let mut peak_seen: f64 = 0.0
  let mut spins: i32 = 0
  while st.stopped_seen == 0 && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.kind == 2 {
          if info.peak > peak_seen { peak_seen = info.peak }
        }
        if info.kind == 4 { st.stopped_seen = 1 }
      },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { spins = 60000 },
    }
  }
  if st.stopped_seen == 0 { ret 60 }
  // The very first block already carries the note.
  if peak_seen <= 0.0 { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 62 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "latency session exited nonzero");

    let samples = read_wav_samples(&temp.path().join("latency.wav"));
    assert_eq!(samples.len(), 256, "expected exactly one block");

    // The attack starts at level 0, so sample 0 is silent by construction;
    // the note must be audible within this single 256-frame block.
    let first_audible = samples.iter().position(|&s| s != 0);
    let idx = first_audible.expect("the note never sounded in the first block");
    assert!(
        idx < 256,
        "note first sounded at frame {}, beyond the first block",
        idx
    );
}

/// Live-output smoke test. Needs a real audio device, so it only runs with
/// `GLYPH_AUDIO_LIVE_TEST=1`; CI excludes it by default.
#[test]
fn live_output_smoke() {
    if std::env::var("GLYPH_AUDIO_LIVE_TEST").as_deref() != Ok("1") {
        return;
    }

    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let mut cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  // Live: the device is opened INSIDE the engine thread; no WAV handle.
  let kind: u32 = sink_live()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, 0 - 1, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  // Warm up first. The engine opens the AudioQueue itself and that takes
  // real time; starting the clock before the device is up would measure
  // the device coming alive, not the engine answering a command. Wait
  // until blocks are actually streaming, THEN time a NoteOn.
  let mut warm: i32 = 0
  let mut spins: i32 = 0
  while warm < 10 && spins < 20000 {
    let got0 = rep_rx.try_recv()
    match got0 {
      Value(rep0) => {
        let info0 = rep_info(rep0)
        if info0.kind == 2 { warm = warm + 1 }
      },
      Empty => { let _sl0 = sleep_ms(1) spins = spins + 1 },
      Disconnected => { spins = 20000 },
    }
  }
  if warm < 10 { ret 59 }

  let sent_at: Instant = now_monotonic()
  let n1 = cmd_tx.try_send(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: 69, velocity: 0.8 }))
  match n1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }

  let mut heard_at_ms: u64 = 99999
  let mut blocks: i32 = 0
  let mut spins2: i32 = 0
  while blocks < 60 && spins2 < 20000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => {
        let info = rep_info(rep)
        if info.kind == 2 {
          blocks = blocks + 1
          if info.peak > 0.0 {
            if heard_at_ms == 99999 { heard_at_ms = sent_at.elapsed_ms() }
          }
        }
      },
      Empty => { let _sl = sleep_ms(1) spins2 = spins2 + 1 },
      Disconnected => { spins2 = 20000 },
    }
  }
  if blocks < 60 { ret 60 }
  // A block is 5.33 ms and the device keeps a few queued, so the note goes
  // audible within a small number of blocks of the command being sent.
  if heard_at_ms > 100 { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 62 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "live output smoke test failed");
}

/// GLYPH-62: the shipped demo builds from a fresh checkout and its offline
/// render is byte-identical to the release gate's canonical render.
///
/// This is what stops the flagship example from quietly rotting: the demo
/// and the gate read the same `song.json` and must produce the same audio,
/// so a change that breaks one breaks the other.
#[test]
fn example_offline_render_matches_the_canonical_gate() {
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }

    let example_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sequencer");
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Copy the shipped example verbatim — no test-only substitutions.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::copy(example_dir.join("glyph.toml"), root.join("glyph.toml")).unwrap();
    fs::copy(example_dir.join("song.json"), root.join("song.json")).unwrap();
    for entry in fs::read_dir(example_dir.join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("glyph") {
            fs::copy(&path, root.join("src").join(path.file_name().unwrap())).unwrap();
        }
    }

    let glyph_bin = env!("CARGO_BIN_EXE_glyph");
    let build = Command::new(glyph_bin)
        .arg("build")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "the shipped example failed to build:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = root.join("target/debug/sequencer");
    assert!(exe.exists(), "expected {}", exe.display());
    let run = Command::new(&exe)
        .args(["--offline", "demo.wav"])
        .current_dir(root)
        .output()
        .unwrap();
    if let Some(sig) = run.status.signal() {
        panic!(
            "example was killed by signal {}\nstderr: {}",
            sig,
            String::from_utf8_lossy(&run.stderr)
        );
    }
    assert_eq!(
        run.status.code(),
        Some(0),
        "example exited nonzero\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let bytes = fs::read(root.join("demo.wav")).unwrap();
    assert_eq!(
        bytes.len(),
        44 + 384_000 * 2,
        "the demo did not render 4 bars"
    );
    let hash = fnv1a64(&bytes);
    assert_eq!(
        hash, EXPECTED_FNV1A,
        "the demo's offline render ({:#018x}) no longer matches the canonical \
         gate ({:#018x}) — the example and the acceptance fixture have drifted",
        hash, EXPECTED_FNV1A
    );
}
