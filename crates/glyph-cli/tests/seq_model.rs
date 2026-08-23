//! GLYPH-55: integration tests for the sequencer control-thread data model
//! (`examples/sequencer/src/seq_model.glyph`). Each test builds a fresh
//! temp `glyph` project, copies the model module into `src/`, writes a
//! test-specific `src/main.glyph` that imports from it, builds and runs
//! the real toolchain end to end, and checks the process exit code.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const SEQ_MODEL_SRC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/sequencer/src/seq_model.glyph"
);

fn skip_run() -> bool {
    std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok()
}

/// Creates a temp `glyph` project named `name` with `seq_model.glyph`
/// copied into `src/` alongside a `src/main.glyph` containing `main_src`.
fn setup_project(name: &str, main_src: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::write(
        root.join("glyph.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"

[[bin]]
name = "{name}"
path = "src/main.glyph"
"#
        ),
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::copy(SEQ_MODEL_SRC, root.join("src/seq_model.glyph")).unwrap();
    fs::write(root.join("src/main.glyph"), main_src).unwrap();

    temp
}

/// Builds the project at `root`, asserting success (printing stderr on
/// failure), then runs its binary `name` and returns the process exit code.
fn build_and_run(root: &Path, name: &str) -> i32 {
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

    let exe = root.join("target/debug").join(name);
    assert!(exe.exists(), "expected built binary at {}", exe.display());

    let run = Command::new(&exe).output().unwrap();
    run.status.code().or_else(|| { use std::os::unix::process::ExitStatusExt; run.status.signal().map(|s| -s) }).unwrap_or(-1)
}

#[test]
fn midi_pitch_conversion() {
    if skip_run() {
        return;
    }

    let main_src = r#"from std/math import fabs
from seq_model import midi_to_freq

fn main() -> i32 {
  let a4 = midi_to_freq(69)
  if a4 != 440.0 { ret 1 }

  let c4 = midi_to_freq(60)
  if fabs(c4 - 261.6256) > 0.001 { ret 2 }

  // pitch 33 is below A4 (69): regression test for the u32 subtraction
  // underflow that midi_to_freq's cast-before-subtract avoids (ADR §8).
  let low = midi_to_freq(33)
  if fabs(low - 55.0) > 0.001 { ret 3 }

  ret 0
}
"#;

    let project = setup_project("midipitch", main_src);
    let code = build_and_run(project.path(), "midipitch");
    assert_eq!(code, 0, "midi_pitch_conversion exited with code {code}");
}

#[test]
fn json_round_trip() {
    if skip_run() {
        return;
    }

    let main_src = r#"from std/vec import Vec
from seq_model import TrackParams, NoteEvent, SongMeta, song_to_json, song_from_json

fn main() -> i32 {
  let mut tracks: Vec<TrackParams> = Vec::new()
  tracks.push(TrackParams { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 })
  tracks.push(TrackParams { track: 1, waveform: 2, gain: 0.6, attack_ms: 2.0, decay_ms: 30.0, sustain: 0.5, release_ms: 120.0 })

  let mut notes: Vec<NoteEvent> = Vec::new()
  notes.push(NoteEvent { track: 0, tick: 0, length_ticks: 480, pitch: 60, velocity: 0.9 })
  notes.push(NoteEvent { track: 0, tick: 480, length_ticks: 480, pitch: 64, velocity: 0.8 })
  notes.push(NoteEvent { track: 1, tick: 960, length_ticks: 960, pitch: 33, velocity: 0.7 })

  let json1 = song_to_json(120.0, 960, 3840, &tracks, &notes)
  let json1_copy = json1.clone()

  let mut tracks2: Vec<TrackParams> = Vec::new()
  let mut notes2: Vec<NoteEvent> = Vec::new()
  let meta = song_from_json(json1, &mut tracks2, &mut notes2)
  if meta.ok != 1 { ret 1 }
  if meta.bpm != 120.0 { ret 2 }
  if meta.ppq != 960 { ret 3 }
  if meta.length_ticks != 3840 { ret 4 }
  if tracks2.len() != 2 { ret 5 }
  if notes2.len() != 3 { ret 6 }

  let json2 = song_to_json(meta.bpm, meta.ppq, meta.length_ticks, &tracks2, &notes2)
  if json1_copy != json2 { ret 7 }

  // Checksum: sum of pitches, sum of ticks must survive the round trip.
  let mut pitch_sum1: u32 = 0
  let mut tick_sum1: u32 = 0
  let mut k: i32 = 0
  while k < notes.len() as i32 {
    pitch_sum1 = pitch_sum1 + notes[k as usize].pitch
    tick_sum1 = tick_sum1 + notes[k as usize].tick
    k = k + 1
  }
  let mut pitch_sum2: u32 = 0
  let mut tick_sum2: u32 = 0
  let mut m: i32 = 0
  while m < notes2.len() as i32 {
    pitch_sum2 = pitch_sum2 + notes2[m as usize].pitch
    tick_sum2 = tick_sum2 + notes2[m as usize].tick
    m = m + 1
  }
  if pitch_sum1 != pitch_sum2 { ret 8 }
  if tick_sum1 != tick_sum2 { ret 9 }

  ret 0
}
"#;

    let project = setup_project("jsonroundtrip", main_src);
    let code = build_and_run(project.path(), "jsonroundtrip");
    assert_eq!(code, 0, "json_round_trip exited with code {code}");
}

#[test]
fn json_rejects_malformed() {
    if skip_run() {
        return;
    }

    let main_src = r#"from std/vec import Vec
from seq_model import TrackParams, NoteEvent, SongMeta, song_from_json

fn main() -> i32 {
  let mut tracks1: Vec<TrackParams> = Vec::new()
  let mut notes1: Vec<NoteEvent> = Vec::new()
  let meta1 = song_from_json("not json", &mut tracks1, &mut notes1)
  if meta1.ok != 0 { ret 1 }
  if tracks1.len() != 0 { ret 2 }
  if notes1.len() != 0 { ret 3 }

  let mut tracks2: Vec<TrackParams> = Vec::new()
  let mut notes2: Vec<NoteEvent> = Vec::new()
  let meta2 = song_from_json("{}", &mut tracks2, &mut notes2)
  if meta2.ok != 0 { ret 4 }
  if tracks2.len() != 0 { ret 5 }
  if notes2.len() != 0 { ret 6 }

  let mut tracks3: Vec<TrackParams> = Vec::new()
  let mut notes3: Vec<NoteEvent> = Vec::new()
  let meta3 = song_from_json("{\"bpm\":\"x\"}", &mut tracks3, &mut notes3)
  if meta3.ok != 0 { ret 7 }
  if tracks3.len() != 0 { ret 8 }
  if notes3.len() != 0 { ret 9 }

  ret 0
}
"#;

    let project = setup_project("jsonmalformed", main_src);
    let code = build_and_run(project.path(), "jsonmalformed");
    assert_eq!(code, 0, "json_rejects_malformed exited with code {code}");
}

#[test]
fn waveform_codes_round_trip() {
    if skip_run() {
        return;
    }

    let main_src = r#"from seq_model import waveform_code, waveform_from_code

fn main() -> i32 {
  if waveform_code(waveform_from_code(0)) != 0 { ret 1 }
  if waveform_code(waveform_from_code(1)) != 1 { ret 2 }
  if waveform_code(waveform_from_code(2)) != 2 { ret 3 }
  if waveform_code(waveform_from_code(3)) != 3 { ret 4 }

  // Unknown code maps to SineWave (0).
  if waveform_code(waveform_from_code(9)) != 0 { ret 5 }

  ret 0
}
"#;

    let project = setup_project("waveformcodes", main_src);
    let code = build_and_run(project.path(), "waveformcodes");
    assert_eq!(code, 0, "waveform_codes_round_trip exited with code {code}");
}
