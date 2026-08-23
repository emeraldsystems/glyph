//! GLYPH-56: integration tests for the voice pool + synth engine
//! (`examples/sequencer/src/seq_voice.glyph`). Each test builds a small
//! throwaway Glyph project containing a copy of the module plus a
//! test-specific `main.glyph`, builds it through the real `glyph` project
//! tool (glyph.toml + `glyph build`, mirroring
//! `crates/glyph-cli/tests/glyph_toml_link.rs`), runs the binary, and
//! (for the tests that render audio) inspects the resulting WAV byte-level
//! (mirroring `crates/glyph-cli/tests/std_audio.rs`).

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const SEQ_VOICE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_voice.glyph");

/// Builds a temp glyph project containing `seq_voice.glyph` plus `main_src`,
/// via the `glyph` project tool, then runs the resulting binary with the
/// project root as cwd (so any WAV files it writes land there). Returns
/// `None` in place of the exit code when execution is skipped
/// (`GLYPH_SKIP_RUN` / `GLYPH_SKIP_RUN_MAIN`), in which case the returned
/// `TempDir` holds no useful output.
fn build_and_run(main_src: &str) -> (Option<i32>, TempDir) {
    let temp = TempDir::new().unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return (None, temp);
    }

    let root = temp.path();

    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "seqvoice"
version = "0.1.0"

[[bin]]
name = "seqvoice"
path = "src/main.glyph"
"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/seq_voice.glyph"), SEQ_VOICE_SRC).unwrap();
    fs::write(root.join("src/main.glyph"), main_src).unwrap();

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

    let exe = root.join("target/debug/seqvoice");
    assert!(exe.exists(), "expected built binary at {}", exe.display());

    let run = Command::new(&exe).current_dir(root).output().unwrap();
    (run.status.code(), temp)
}

/// Parses a WAV file written by `std/audio`'s `WavWriter` (44-byte header,
/// little-endian 16-bit PCM mono) into its samples.
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

#[test]
fn silence_renders_exact_zeros() {
    let main_src = r#"
        import std
        from seq_voice import voices_init, voices_render_block
        from std/audio import WavWriter, wav_create
        from std/enums import Result
        from std/vec import Vec

        fn main() -> i32 {
          let mut vi: Vec<u32> = Vec::new()
          let mut vf: Vec<f64> = Vec::new()
          let mut vt: Vec<u64> = Vec::new()
          let _init = voices_init(&mut vi, &mut vf, &mut vt)

          let r = wav_create("silence.wav", 48000, 1)
          ret match r {
            Ok(w) => {
              let mut writer = w
              let mut b: i32 = 0
              while b < 10 {
                let blk: Vec<f64> = voices_render_block(&mut vi, &mut vf, 256)
                match writer.write(&blk) {
                  Ok(_n) => {},
                  Err(_e) => { ret 2 },
                }
                b = b + 1
              }
              match writer.close() {
                Ok(_u) => 0,
                Err(_e2) => 3,
              }
            },
            Err(_e0) => 1,
          }
        }
    "#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "program exited nonzero");

    let samples = read_wav_samples(&temp.path().join("silence.wav"));
    assert_eq!(samples.len(), 10 * 256, "unexpected sample count");
    assert!(
        samples.iter().all(|&s| s == 0),
        "expected all-zero silence with no voices active"
    );
}

#[test]
fn single_note_envelope_shape() {
    let main_src = r#"
        import std
        from seq_voice import voices_init, voices_note_on, voices_render_block
        from std/audio import WavWriter, wav_create
        from std/enums import Result
        from std/vec import Vec

        fn main() -> i32 {
          let mut vi: Vec<u32> = Vec::new()
          let mut vf: Vec<f64> = Vec::new()
          let mut vt: Vec<u64> = Vec::new()
          let _init = voices_init(&mut vi, &mut vf, &mut vt)
          let _n = voices_note_on(&mut vi, &mut vf, &mut vt, 0, 0, 69, 1.0, 0, 0.5, 10.0, 20.0, 0.6, 30.0)

          let r = wav_create("envelope.wav", 48000, 1)
          ret match r {
            Ok(w) => {
              let mut writer = w
              let mut b: i32 = 0
              while b < 187 {
                let blk: Vec<f64> = voices_render_block(&mut vi, &mut vf, 256)
                match writer.write(&blk) {
                  Ok(_n2) => {},
                  Err(_e) => { ret 2 },
                }
                b = b + 1
              }
              let last: Vec<f64> = voices_render_block(&mut vi, &mut vf, 128)
              match writer.write(&last) {
                Ok(_n3) => {},
                Err(_e2) => { ret 4 },
              }
              match writer.close() {
                Ok(_u) => 0,
                Err(_e3) => 3,
              }
            },
            Err(_e0) => 1,
          }
        }
    "#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "program exited nonzero");

    // pitch 69 = A4 = 440 Hz, sine, velocity 1.0, gain 0.5,
    // attack 10ms / decay 20ms / sustain 0.6 / release 30ms.
    let samples = read_wav_samples(&temp.path().join("envelope.wav"));
    assert_eq!(samples.len(), 48000, "unexpected sample count");

    assert_eq!(samples[0], 0, "attack must start at level 0");

    // Sustain region: attack (10ms=480 frames) + decay (20ms=960 frames)
    // complete well before frame 3000, so [3000, 4000) is fully settled.
    let sustain_peak = samples[3000..4000]
        .iter()
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap();
    let expected_sustain = 0.5 * 0.6 * 32767.0;
    let tolerance = expected_sustain * 0.03;
    assert!(
        (sustain_peak as f64 - expected_sustain).abs() <= tolerance,
        "sustain peak {} not within 3% of expected {}",
        sustain_peak,
        expected_sustain
    );

    // Global peak occurs somewhere in the attack ramp, when level hits 1.0
    // at a favorable point in the sine cycle. Window is generous because
    // the exact peak sample depends on where in the waveform that lands.
    let global_peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    let upper = (0.5 * 32767.0) as u16 + 1;
    let lower = (0.45 * 32767.0) as u16;
    assert!(
        global_peak <= upper && global_peak >= lower,
        "global peak {} outside expected [{}, {}]",
        global_peak,
        lower,
        upper
    );
}

#[test]
fn note_off_then_silence() {
    let main_src = r#"
        import std
        from seq_voice import voices_init, voices_note_on, voices_note_off, voices_render_block
        from std/audio import WavWriter, wav_create
        from std/enums import Result
        from std/vec import Vec

        fn main() -> i32 {
          let mut vi: Vec<u32> = Vec::new()
          let mut vf: Vec<f64> = Vec::new()
          let mut vt: Vec<u64> = Vec::new()
          let _init = voices_init(&mut vi, &mut vf, &mut vt)
          let _n = voices_note_on(&mut vi, &mut vf, &mut vt, 0, 0, 69, 1.0, 0, 0.5, 5.0, 5.0, 0.7, 30.0)

          let r = wav_create("noteoff.wav", 48000, 1)
          ret match r {
            Ok(w) => {
              let mut writer = w

              let mut b1: i32 = 0
              while b1 < 18 {
                let blk: Vec<f64> = voices_render_block(&mut vi, &mut vf, 256)
                match writer.write(&blk) {
                  Ok(_n1) => {},
                  Err(_e) => { ret 2 },
                }
                b1 = b1 + 1
              }
              let rem1: Vec<f64> = voices_render_block(&mut vi, &mut vf, 192)
              match writer.write(&rem1) {
                Ok(_n2) => {},
                Err(_e2) => { ret 2 },
              }

              let _off = voices_note_off(&mut vi, &mut vf, 0, 69)

              let mut b2: i32 = 0
              while b2 < 18 {
                let blk2: Vec<f64> = voices_render_block(&mut vi, &mut vf, 256)
                match writer.write(&blk2) {
                  Ok(_n3) => {},
                  Err(_e3) => { ret 3 },
                }
                b2 = b2 + 1
              }
              let rem2: Vec<f64> = voices_render_block(&mut vi, &mut vf, 192)
              match writer.write(&rem2) {
                Ok(_n4) => {},
                Err(_e4) => { ret 3 },
              }

              match writer.close() {
                Ok(_u) => 0,
                Err(_e5) => 4,
              }
            },
            Err(_e0) => 1,
          }
        }
    "#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "program exited nonzero");

    // note_on at frame 0, note_off at frame 4800; 30ms release = 1440
    // frames, so the voice is silent well before frame 8600.
    let samples = read_wav_samples(&temp.path().join("noteoff.wav"));
    assert_eq!(samples.len(), 9600, "unexpected sample count");

    let tail = &samples[8600..9600];
    assert!(
        tail.iter().all(|&s| s == 0),
        "expected silence in the release tail"
    );
}

#[test]
fn steal_oldest_reuses_voice() {
    let main_src = r#"
        from seq_voice import voices_init, voices_note_on
        from std/vec import Vec

        fn main() -> i32 {
          let mut vi: Vec<u32> = Vec::new()
          let mut vf: Vec<f64> = Vec::new()
          let mut vt: Vec<u64> = Vec::new()
          let _init = voices_init(&mut vi, &mut vf, &mut vt)

          let mut first_idx: i32 = -1
          let mut n: u32 = 0
          let mut fr: u64 = 0
          while n < 16 {
            let got: i32 = voices_note_on(&mut vi, &mut vf, &mut vt, fr, 0, 60 + n, 1.0, 0, 1.0, 5.0, 5.0, 0.8, 5.0)
            if n == 0 { first_idx = got }
            n = n + 1
            fr = fr + 1
          }
          let seventeenth: i32 = voices_note_on(&mut vi, &mut vf, &mut vt, fr, 0, 90, 1.0, 0, 1.0, 5.0, 5.0, 0.8, 5.0)
          if seventeenth != first_idx { ret 1 }
          ret 0
        }
    "#;

    let (code, _temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(
        code, 0,
        "17th note_on did not steal the voice with the oldest started_frame"
    );
}

#[test]
fn render_is_deterministic() {
    let main_src = r#"
        import std
        from seq_voice import voices_init, voices_note_on, voices_render_block
        from std/audio import WavWriter, wav_create
        from std/enums import Result
        from std/vec import Vec

        fn render_scene(path: str) -> i32 {
          let mut vi: Vec<u32> = Vec::new()
          let mut vf: Vec<f64> = Vec::new()
          let mut vt: Vec<u64> = Vec::new()
          let _init = voices_init(&mut vi, &mut vf, &mut vt)
          let _n1 = voices_note_on(&mut vi, &mut vf, &mut vt, 0, 0, 60, 0.9, 0, 0.4, 8.0, 15.0, 0.5, 20.0)
          let _n2 = voices_note_on(&mut vi, &mut vf, &mut vt, 480, 1, 67, 0.7, 2, 0.3, 5.0, 10.0, 0.6, 25.0)

          let r = wav_create(path, 48000, 1)
          ret match r {
            Ok(w) => {
              let mut writer = w
              let mut b: i32 = 0
              while b < 20 {
                let blk: Vec<f64> = voices_render_block(&mut vi, &mut vf, 256)
                match writer.write(&blk) {
                  Ok(_n3) => {},
                  Err(_e) => { ret 2 },
                }
                b = b + 1
              }
              match writer.close() {
                Ok(_u) => 0,
                Err(_e2) => 3,
              }
            },
            Err(_e0) => 1,
          }
        }

        fn main() -> i32 {
          let s1 = render_scene("det1.wav")
          if s1 != 0 { ret s1 }
          let s2 = render_scene("det2.wav")
          if s2 != 0 { ret s2 + 10 }
          ret 0
        }
    "#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "program exited nonzero");

    let bytes1 = fs::read(temp.path().join("det1.wav")).unwrap();
    let bytes2 = fs::read(temp.path().join("det2.wav")).unwrap();
    assert_eq!(bytes1, bytes2, "expected byte-identical WAV renders");
}
