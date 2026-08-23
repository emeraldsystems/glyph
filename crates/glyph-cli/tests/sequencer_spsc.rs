//! GLYPH-46: a sequencer worker sends bounded render events through SPSC;
//! the ordinary render loop drains them and writes a deterministic WAV.

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
#[cfg(all(feature = "codegen", unix))]
use glyph_frontend::{FrontendOptions, compile_source};
#[cfg(all(feature = "codegen", unix))]
use std::{os::unix::process::ExitStatusExt, process::Command};
#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_in(temp: &TempDir, source: &str) -> Option<i32> {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return None;
    }
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "sequencer source failed: {:?}",
        output.diagnostics
    );
    let object = temp.path().join("sequencer.o");
    let executable = temp.path().join("sequencer");
    let mut context = CodegenContext::new("sequencer_spsc").unwrap();
    context.codegen_module(&output.mir).unwrap();
    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return None;
    }
    context.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object],
            link_libs: Vec::new(),
            link_search_paths: Vec::new(),
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();
    let status = Command::new(executable)
        .current_dir(temp.path())
        .status()
        .unwrap();
    Some(if let Some(code) = status.code() {
        code
    } else if let Some(signal) = status.signal() {
        -signal
    } else {
        -1
    })
}

#[cfg(all(feature = "codegen", unix))]
const OFFLINE_SEQUENCER: &str = include_str!("../../../tests/fixtures/sequencer/spsc_proof.glyph");

#[cfg(all(feature = "codegen", unix))]
#[test]
fn offline_threaded_sequencer_wav_is_deterministic() {
    let temp = TempDir::new().unwrap();
    let Some(exit) = build_and_run_in(&temp, OFFLINE_SEQUENCER) else {
        return;
    };
    assert_eq!(exit, 0);

    let wav = std::fs::read(temp.path().join("sequencer.wav")).unwrap();
    assert_eq!(wav.len(), 44 + 48_000 * 2);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);

    let samples: Vec<i16> = wav[44..]
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    assert!(samples[12_000..24_000].iter().all(|sample| *sample == 0));
    assert!(samples[36_000..48_000].iter().all(|sample| *sample == 0));
    let first_crossings = samples[..12_000]
        .windows(2)
        .filter(|pair| pair[0] < 0 && pair[1] >= 0)
        .count();
    let second_crossings = samples[24_000..36_000]
        .windows(2)
        .filter(|pair| pair[0] < 0 && pair[1] >= 0)
        .count();
    assert!((109..=110).contains(&first_crossings));
    assert!((164..=165).contains(&second_crossings));
}

// Opens a real device and is therefore opt-in. The rendered Glyph loop writes
// through the ordinary blocking AudioOut API; the platform callback only
// consumes native buffers and never invokes Glyph code.
#[cfg(all(feature = "codegen", unix))]
const LIVE_SEQUENCER: &str = r#"
from std/audio import AudioOut, out_open
from std/math import sin, tau
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
from std/time import Instant, now_monotonic, sleep_until_ns
from std/thread import spawn
from std/vec import Vec

struct RenderEvent {
  at_frame: i32,
  frames: i32,
  frequency: f64,
  gain: f64,
  deadline_ns: u64,
  produced_ns: u64,
  queue_depth: i32
}

fn main() -> i32 {
  let mut receiver: Receiver<RenderEvent>
  let sender: Sender<RenderEvent> = channel(2, &mut receiver)
  let task: FnOnce<(), i32> = move () -> {
    let clock: Instant = now_monotonic()
    let deadline_ns = clock.as_nanos() + 5000000
    match sleep_until_ns(deadline_ns) {
      Ok(_unit) => {},
      Err(_error) => { ret 1 },
    }
    let produced_at: Instant = now_monotonic()
    let produced_ns = produced_at.as_nanos()
    let result = sender.try_send(RenderEvent {
      at_frame: 0,
      frames: 4800,
      frequency: 440.0,
      gain: 0.1,
      deadline_ns: deadline_ns,
      produced_ns: produced_ns,
      queue_depth: 1
    })
    ret match result { Sent => 0, Full(_event) => 2, Disconnected(_event) => 3 }
  }
  let started = spawn(task)
  let status = match started {
    Ok(handle) => match handle.join() { Ok(code) => code, Err(_error) => 4 },
    Err(_error) => 5,
  }
  if status != 0 { ret status }

  let mut samples: Vec<f64> = Vec::new()
  let event = match receiver.try_recv() {
    Value(value) => value,
    Empty => { ret 6 },
    Disconnected => { ret 7 },
  }
  let scheduling_jitter_ns = if event.produced_ns > event.deadline_ns {
    event.produced_ns - event.deadline_ns
  } else {
    0
  }
  let mut frame: i32 = 0
  while frame < event.frames {
    let phase = frame as f64 * tau() * event.frequency / 48000.0
    samples.push(sin(phase) * event.gain)
    frame = frame + 1
  }

  ret match out_open(48000, 1) {
    Ok(device) => {
      let mut output = device
      match output.write(&samples) {
        Ok(_count) => match output.close() {
          Ok(_unit) => 0,
          Err(_error) => 9,
        },
        Err(_error) => 8,
      }
    },
    Err(_error) => 10,
  }
}
"#;

// CI validates the complete frontend and backend lowering without opening a
// device. This catches live-demo language drift on non-macOS builders.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn live_threaded_sequencer_source_compiles_without_device() {
    let output = compile_source(
        LIVE_SEQUENCER,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "live sequencer source failed: {:?}",
        output.diagnostics
    );
    let mut context = CodegenContext::new("live_sequencer_spsc").unwrap();
    context.codegen_module(&output.mir).unwrap();
}

#[cfg(all(feature = "codegen", target_os = "macos"))]
#[test]
fn live_threaded_sequencer_demo_is_environment_gated() {
    if std::env::var("GLYPH_AUDIO_LIVE_TEST").is_err() {
        return;
    }
    let temp = TempDir::new().unwrap();
    if let Some(exit) = build_and_run_in(&temp, LIVE_SEQUENCER) {
        assert_eq!(exit, 0);
    }
}
