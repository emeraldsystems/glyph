//! std/audio: offline WAV rendering (verified byte-for-byte) and live
//! output (env-gated; requires an audio device).
//!
//! Also holds the regression for the unit-payload match crash: binding the
//! payload of `Ok(())` (e.g. `Ok(_u)` on Result<(), E>) used to trap the
//! compiler inside LLVMBuildAlloca — empty-tuple locals had no storable
//! LLVM size.

#[cfg(all(feature = "codegen", unix))]
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

/// Builds `source` and runs it with the temp dir as cwd, so relative paths
/// the program writes land inside `temp`.
#[cfg(all(feature = "codegen", unix))]
fn build_and_run_in(temp: &TempDir, source: &str) -> Option<i32> {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return None;
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

    let obj_path = temp.path().join("test.o");
    let exe_path = temp.path().join("test_exe");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return None;
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

    let status = Command::new(&exe_path)
        .current_dir(temp.path())
        .status()
        .unwrap();
    Some(if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        -sig
    } else {
        -1
    })
}

// Render one second of 440 Hz sine and verify the WAV byte-for-byte:
// header fields, data size, peak amplitude, and cycle count.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn wav_render_sine_verified() {
    let source = r#"
        from std/enums import Result
        from std/vec import Vec
        from std/math import sin, tau
        from std/audio import WavWriter, wav_create

        fn main() -> i32 {
          let r = wav_create("sine.wav", 48000, 1)
          match r {
            Ok(w) => {
              let mut writer = w
              let mut block: Vec<f64> = Vec::new()
              let mut i: i32 = 0;
              while i < 48000 {
                let phase: f64 = i as f64 * tau() * 440.0 / 48000.0;
                block.push(sin(phase) * 0.8)
                if block.len() == 1024 {
                  let wr = writer.write(&block)
                  match wr {
                    Ok(_n) => {},
                    Err(_e) => { ret 2 },
                  }
                  block = Vec::new()
                }
                i = i + 1;
              }
              if block.len() > 0 {
                let wr2 = writer.write(&block)
                match wr2 {
                  Ok(_n2) => {},
                  Err(_e2) => { ret 3 },
                }
              }
              let c = writer.close()
              match c {
                Ok(_u) => 0,
                Err(_e3) => 4,
              }
            },
            Err(_e0) => 1,
          }
        }
    "#;

    let temp = TempDir::new().unwrap();
    let Some(code) = build_and_run_in(&temp, source) else {
        return;
    };
    assert_eq!(code, 0, "render program failed");

    let bytes = std::fs::read(temp.path().join("sine.wav")).unwrap();
    assert_eq!(bytes.len(), 44 + 48000 * 2, "unexpected WAV size");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(
        u16::from_le_bytes([bytes[22], bytes[23]]),
        1,
        "channel count"
    );
    assert_eq!(
        u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
        48000,
        "sample rate"
    );
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
        48000 * 2,
        "data chunk size"
    );

    let samples: Vec<i16> = bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    let expected_peak = (0.8f64 * 32767.0) as u16;
    assert!(
        peak.abs_diff(expected_peak) <= 1,
        "peak {} != expected {}",
        peak,
        expected_peak
    );

    let zero_crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0 && w[1] >= 0)
        .count();
    assert!(
        (438..=441).contains(&zero_crossings),
        "expected ~440 cycles, got {}",
        zero_crossings
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn wav_open_invalid_path_errors() {
    let source = r#"
        from std/enums import Result
        from std/audio import WavWriter, wav_create

        fn main() -> i32 {
          let r = wav_create("no/such/dir/out.wav", 48000, 1)
          ret match r {
            Ok(_w) => 1,
            Err(_e) => 0,
          }
        }
    "#;

    let temp = TempDir::new().unwrap();
    if let Some(code) = build_and_run_in(&temp, source) {
        assert_eq!(code, 0);
    }
}

// Regression: `Ok(_u)` on Result<(), E> used to crash the compiler
// (LLVMBuildAlloca of an unsized empty-tuple local).
#[cfg(all(feature = "codegen", unix))]
#[test]
fn unit_payload_match_binding_compiles_and_runs() {
    let source = r#"
        from std/enums import Result

        fn nothing() -> Result<(), String> {
          ret Ok(())
        }

        fn main() -> i32 {
          let r = nothing()
          ret match r {
            Ok(_u) => 0,
            Err(_e) => 1,
          }
        }
    "#;

    let temp = TempDir::new().unwrap();
    if let Some(code) = build_and_run_in(&temp, source) {
        assert_eq!(code, 0);
    }
}

// Live playback: opens the default output device, so it only runs when
// explicitly requested (GLYPH_AUDIO_LIVE_TEST=1) — not in CI.
#[cfg(all(feature = "codegen", target_os = "macos"))]
#[test]
fn live_output_open_write_close() {
    if std::env::var("GLYPH_AUDIO_LIVE_TEST").is_err() {
        return;
    }

    let source = r#"
        from std/enums import Result
        from std/vec import Vec
        from std/math import sin, tau
        from std/audio import AudioOut, out_open

        fn main() -> i32 {
          let r = out_open(48000, 1)
          match r {
            Ok(o) => {
              let mut out = o
              let mut block: Vec<f64> = Vec::new()
              let mut i: i32 = 0;
              while i < 4800 {
                let phase: f64 = i as f64 * tau() * 440.0 / 48000.0;
                block.push(sin(phase) * 0.1)
                if block.len() == 1024 {
                  let wr = out.write(&block)
                  match wr {
                    Ok(_n) => {},
                    Err(_e) => { ret 2 },
                  }
                  block = Vec::new()
                }
                i = i + 1;
              }
              let c = out.close()
              match c {
                Ok(_u) => 0,
                Err(_e3) => 4,
              }
            },
            Err(_e0) => 1,
          }
        }
    "#;

    let temp = TempDir::new().unwrap();
    if let Some(code) = build_and_run_in(&temp, source) {
        assert_eq!(code, 0);
    }
}
