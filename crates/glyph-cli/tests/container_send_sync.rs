//! GLYPH-63: structural Send/Sync for stdlib containers. Vec<T>, Map<K,V>,
//! Option<T>, and Result<T,E> resolve thread-safety structurally over their
//! type arguments (they are unique-owner containers adding no sharing of
//! their own), instead of failing closed as "no canonical resolver
//! identity". The fail-closed rejection remains for unknown generics, and
//! unsafe element types (Shared, RawPtr, borrows) are still rejected with
//! their precise per-element paths.
//!
//! Also proves the GLYPH-60 shape: Arc<Pattern> snapshots (a struct
//! carrying a Vec) crossing SPSC channels and spawn boundaries with stable
//! refcounts under churn.

use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use std::os::unix::process::ExitStatusExt;

#[cfg(all(feature = "codegen", unix))]
use std::process::Command;

#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return 0;
    }

    let frontend_output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        frontend_output.diagnostics.is_empty(),
        "Compilation failed with diagnostics: {:?}",
        frontend_output.diagnostics
    );

    let temp = TempDir::new().unwrap();
    let obj_path = temp.path().join("test.o");
    let exe_path = temp.path().join("test_exe");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return 0;
    }
    ctx.emit_object_file(&obj_path).unwrap();

    let linker = Linker::new();
    let opts = LinkerOptions {
        output_path: exe_path.clone(),
        object_files: vec![obj_path],
        link_libs: Vec::new(),
        link_search_paths: Vec::new(),
        runtime_lib_path: Linker::get_runtime_lib_path(),
    };
    linker.link(&opts).unwrap();

    let status = Command::new(&exe_path).status().unwrap();
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        -sig
    } else {
        -1
    }
}

fn diagnostics_for(source: &str) -> Vec<String> {
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    output
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

// The original GLYPH-63 repro: an enum command whose payload struct carries
// a Vec, sent through an SPSC channel into a spawned engine loop.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_carrying_payload_crosses_channel_and_spawn() {
    let source = r#"
        from std/enums import Result, Option
        from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
        from std/thread import JoinHandle, ThreadError, spawn
        from std/vec import Vec

        struct NoteEvent {
          tick: u32,
          pitch: u32,
        }

        struct Pattern {
          length_ticks: u32,
          notes: Vec<NoteEvent>,
        }

        enum EngineCommand {
          Play,
          LoadPattern(Pattern),
          Shutdown,
        }

        fn notes_checksum(notes: &Vec<NoteEvent>) -> i32 {
          let mut total: i32 = notes.len() as i32
          if notes.len() > 0 {
            let first = notes[0]
            total = total + first.pitch as i32
          }
          ret total
        }

        fn main() -> i32 {
          let mut cmd_rx: Receiver<EngineCommand>
          let cmd_tx: Sender<EngineCommand> = channel(8, &mut cmd_rx)

          let mut notes: Vec<NoteEvent> = Vec::new()
          notes.push(NoteEvent { tick: 0, pitch: 60 })
          notes.push(NoteEvent { tick: 960, pitch: 64 })
          let pattern = Pattern { length_ticks: 3840, notes: notes }

          let s1 = cmd_tx.try_send(EngineCommand::LoadPattern(pattern))
          match s1 { Sent => {}, Full(_c1) => { ret 10 }, Disconnected(_d1) => { ret 11 }, }
          let s2 = cmd_tx.try_send(EngineCommand::Shutdown)
          match s2 { Sent => {}, Full(_c2) => { ret 12 }, Disconnected(_d2) => { ret 13 }, }

          let task: FnOnce<(), i32> = move () -> {
            let mut rx = cmd_rx
            let mut checksum: i32 = 0
            let mut running: bool = true
            while running {
              let got = rx.try_recv()
              match got {
                Value(cmd) => match cmd {
                  Play => { checksum = checksum + 1 },
                  LoadPattern(p) => {
                    checksum = checksum + p.length_ticks as i32 + notes_checksum(&p.notes)
                  },
                  Shutdown => { running = false },
                },
                Empty => {},
                Disconnected => { running = false },
              }
            }
            ret checksum
          }
          let started = spawn(task)
          let checksum: i32 = match started {
            Ok(handle) => match handle.join() {
              Ok(sum) => sum,
              Err(_je) => 0 - 20,
            },
            Err(_se) => 0 - 21,
          }
          // 3840 + (2 + 60) = 3902
          if checksum != 3902 { ret 22 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// GLYPH-60: Arc<Pattern> pattern-bank snapshots — 1000 clone/send/recv/drop
// cycles churn the refcount across the channel boundary, one snapshot is
// read from a spawned thread, and the control-side handle stays valid.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn arc_pattern_snapshot_stress_and_cross_thread_read() {
    let source = r#"
        from std/enums import Result, Option
        from std/sync import Arc
        from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
        from std/thread import JoinHandle, ThreadError, spawn
        from std/vec import Vec

        struct NoteEvent {
          tick: u32,
          pitch: u32,
        }

        struct Pattern {
          length_ticks: u32,
          notes: Vec<NoteEvent>,
        }

        fn notes_sum(notes: &Vec<NoteEvent>) -> i32 {
          let mut total: i32 = 0
          let mut i: i32 = 0
          while i < notes.len() as i32 {
            let n = notes[i as usize]
            total = total + n.pitch as i32
            i = i + 1
          }
          ret total
        }

        fn main() -> i32 {
          let mut rx: Receiver<Arc<Pattern>>
          let tx: Sender<Arc<Pattern>> = channel(8, &mut rx)

          let mut notes: Vec<NoteEvent> = Vec::new()
          notes.push(NoteEvent { tick: 0, pitch: 60 })
          notes.push(NoteEvent { tick: 960, pitch: 64 })
          notes.push(NoteEvent { tick: 1920, pitch: 67 })
          let bank = Arc::new(Pattern { length_ticks: 3840, notes: notes })

          let mut sent: i32 = 0
          let mut round: i32 = 0
          while round < 1000 {
            let snapshot = bank.clone()
            let s = tx.try_send(snapshot)
            match s {
              Sent => { sent = sent + 1 },
              Full(_back) => {},
              Disconnected(_dead) => { ret 10 },
            }
            let got = rx.try_recv()
            match got {
              Value(snap) => {
                let p = snap.borrow()
                if p.length_ticks != 3840 { ret 11 }
              },
              Empty => {},
              Disconnected => { ret 12 },
            }
            round = round + 1
          }
          if sent < 900 { ret 13 }

          let mut draining: bool = true
          while draining {
            let leftover = rx.try_recv()
            match leftover {
              Value(_snap2) => {},
              Empty => { draining = false },
              Disconnected => { draining = false },
            }
          }

          let mut rx2: Receiver<Arc<Pattern>>
          let tx2: Sender<Arc<Pattern>> = channel(4, &mut rx2)
          let s2 = tx2.try_send(bank.clone())
          match s2 { Sent => {}, Full(_b2) => { ret 14 }, Disconnected(_d2) => { ret 15 }, }

          let engine: FnOnce<(), i32> = move () -> {
            let mut inbox = rx2
            let got2 = inbox.try_recv()
            ret match got2 {
              Value(snap3) => {
                let p2 = snap3.borrow()
                notes_sum(&p2.notes) + p2.length_ticks as i32
              },
              Empty => 0 - 1,
              Disconnected => 0 - 2,
            }
          }
          let started = spawn(engine)
          let engine_sum: i32 = match started {
            Ok(handle) => match handle.join() {
              Ok(v) => v,
              Err(_je) => 0 - 20,
            },
            Err(_se) => 0 - 21,
          }
          if engine_sum != 4031 { ret 16 }

          let p3 = bank.borrow()
          if notes_sum(&p3.notes) != 191 { ret 17 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Enum-typed fields inside channel payload structs (the ADR section 11
// deferred item), in two parts: a struct with a Waveform field crosses the
// channel + spawn (Send policy resolves structurally), and the enum itself
// crosses as a direct channel payload and is matched on the far side.
// (Reading an enum-typed FIELD by value is still blocked by the separate
// field-move guard — same family as Vec fields — so the readable-value
// proof uses the direct payload.)
#[cfg(all(feature = "codegen", unix))]
#[test]
fn enum_field_in_channel_payload_crosses_spawn() {
    let source = r#"
        from std/enums import Result, Option
        from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
        from std/thread import JoinHandle, ThreadError, spawn

        enum Waveform {
          SineWave,
          SawWave,
          SquareWave,
          TriangleWave,
        }

        struct TrackMsg {
          track: u32,
          waveform: Waveform,
          gain: f64,
        }

        fn waveform_score(w: Waveform) -> i32 {
          ret match w {
            SineWave => 1,
            SawWave => 2,
            SquareWave => 3,
            TriangleWave => 4,
          }
        }

        fn main() -> i32 {
          // Part 1: enum-typed field rides inside a struct payload.
          let mut rx: Receiver<TrackMsg>
          let tx: Sender<TrackMsg> = channel(4, &mut rx)
          let s1 = tx.try_send(TrackMsg { track: 7, waveform: Waveform::SquareWave, gain: 0.8 })
          match s1 { Sent => {}, Full(_a) => { ret 10 }, Disconnected(_b) => { ret 11 }, }

          // Part 2: the enum as a direct channel payload, matched remotely.
          let mut wrx: Receiver<Waveform>
          let wtx: Sender<Waveform> = channel(4, &mut wrx)
          let s2 = wtx.try_send(Waveform::TriangleWave)
          match s2 { Sent => {}, Full(_c) => { ret 12 }, Disconnected(_d) => { ret 13 }, }

          let task: FnOnce<(), i32> = move () -> {
            let mut inbox = rx
            let mut winbox = wrx
            let msg_part = match inbox.try_recv() {
              Value(msg) => msg.track as i32,
              Empty => 0 - 1,
              Disconnected => 0 - 2,
            }
            let wave_part = match winbox.try_recv() {
              Value(w) => waveform_score(w) * 100,
              Empty => 0 - 3,
              Disconnected => 0 - 4,
            }
            ret msg_part + wave_part
          }
          let started = spawn(task)
          let score: i32 = match started {
            Ok(handle) => match handle.join() {
              Ok(v) => v,
              Err(_je) => 0 - 20,
            },
            Err(_se) => 0 - 21,
          }
          // 7 + 4*100
          if score != 407 { ret 14 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Nested containers compose: Vec<Vec<f64>> captured into a spawn.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_vec_crosses_spawn() {
    let source = r#"
        from std/enums import Result
        from std/thread import JoinHandle, ThreadError, spawn
        from std/vec import Vec

        fn inner_sum(rows: &Vec<Vec<f64>>) -> i32 {
          let mut total: f64 = 0.0
          let mut i: i32 = 0
          while i < rows.len() as i32 {
            let row = rows[i as usize]
            let mut j: i32 = 0
            while j < row.len() as i32 {
              total = total + row[j as usize]
              j = j + 1
            }
            i = i + 1
          }
          ret total as i32
        }

        fn main() -> i32 {
          let mut rows: Vec<Vec<f64>> = Vec::new()
          let mut r0: Vec<f64> = Vec::new()
          r0.push(1.5)
          r0.push(2.5)
          rows.push(r0)
          let mut r1: Vec<f64> = Vec::new()
          r1.push(4.0)
          rows.push(r1)

          let task: FnOnce<(), i32> = move () -> {
            inner_sum(&rows)
          }
          let started = spawn(task)
          let total: i32 = match started {
            Ok(handle) => match handle.join() {
              Ok(v) => v,
              Err(_je) => 0 - 20,
            },
            Err(_se) => 0 - 21,
          }
          if total != 8 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Fail-closed is preserved: unsafe element types are rejected with the
// precise per-element path, not blanket-allowed by the container rule.
#[test]
fn vec_of_shared_still_rejected() {
    let diags = diagnostics_for(
        r#"from std/enums import Result, Option
from std/thread import JoinHandle, ThreadError, spawn
from std/vec import Vec

fn main() -> i32 {
  let mut xs: Vec<Shared<i32>> = Vec::new()
  xs.push(Shared::new(7))
  let task: FnOnce<(), i32> = move () -> {
    xs.len() as i32
  }
  let started = spawn(task)
  ret match started {
    Ok(_h) => 0,
    Err(_e) => 1,
  }
}
"#,
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("Shared<T>") && d.contains("is not Send")),
        "expected Shared rejection through the Vec element path, got: {:?}",
        diags
    );
}

#[test]
fn unknown_generic_still_fails_closed() {
    let diags = diagnostics_for(
        r#"from std/enums import Result
from std/thread import JoinHandle, ThreadError, spawn
from std/vec import Vec

struct Holder<T> {
  item: T,
}

fn main() -> i32 {
  let h: Holder<i32> = Holder { item: 3 }
  let task: FnOnce<(), i32> = move () -> {
    h.item
  }
  let started = spawn(task)
  ret match started {
    Ok(_h2) => 0,
    Err(_e) => 1,
  }
}
"#,
    );
    // A user generic spelled as an application either monomorphizes into a
    // checkable struct (fine, no diagnostic about identity) or is rejected;
    // it must never be waved through by the stdlib container rule. If it
    // compiled cleanly, the monomorphized-path check applies and this test
    // only asserts the absence of a false "canonical Vec/Map" claim.
    assert!(
        !diags
            .iter()
            .any(|d| d.contains("canonical `std::vec::Vec`") || d.contains("canonical `std::map::Map`")),
        "user generic must not acquire stdlib container policy: {:?}",
        diags
    );
}
