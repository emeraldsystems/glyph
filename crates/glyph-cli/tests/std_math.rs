//! std/math: libm bindings (f64) plus pi/tau/euler, clamp, and lerp helpers.

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

#[cfg(all(feature = "codegen", unix))]
#[test]
fn libm_functions_runtime() {
    let source = r#"
        from std/math import sin, cos, pow, sqrt, floor, ceil, fabs, fmod, atan2, exp, log, pi

        fn main() -> i32 {
          if sqrt(16.0) != 4.0 { ret 1 }
          if pow(2.0, 10.0) != 1024.0 { ret 2 }
          if floor(3.7) != 3.0 { ret 3 }
          if ceil(3.2) != 4.0 { ret 4 }
          if fabs(-2.5) != 2.5 { ret 5 }
          if fmod(7.5, 2.0) != 1.5 { ret 6 }
          if sin(0.0) != 0.0 { ret 7 }
          if cos(0.0) != 1.0 { ret 8 }

          let p: f64 = pi();
          let s: f64 = sin(p / 2.0);
          if fabs(s - 1.0) > 0.0000001 { ret 9 }

          let angle: f64 = atan2(1.0, 1.0);
          if fabs(angle - p / 4.0) > 0.0000001 { ret 10 }

          if fabs(log(exp(1.0)) - 1.0) > 0.0000001 { ret 11 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn math_helpers_runtime() {
    let source = r#"
        from std/math import lerp, clamp, fmin, fmax, pi, tau

        fn main() -> i32 {
          if lerp(0.0, 8.0, 0.25) != 2.0 { ret 1 }
          if clamp(5.0, 0.0, 1.0) != 1.0 { ret 2 }
          if clamp(-1.0, 0.0, 1.0) != 0.0 { ret 3 }
          if clamp(0.5, 0.0, 1.0) != 0.5 { ret 4 }
          if fmin(2.0, 3.0) != 2.0 { ret 5 }
          if fmax(2.0, 3.0) != 3.0 { ret 6 }
          if tau() != pi() * 2.0 { ret 7 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// The oscillator inner-loop shape the synth work needs: a sine table
// computed with std/math, phase in samples, casts for normalization.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn sine_oscillator_block_runtime() {
    let source = r#"
        from std/vec import Vec
        from std/math import sin, fabs, tau

        fn main() -> i32 {
          let sample_rate: f64 = 48000.0;
          let freq: f64 = 480.0;
          let mut block: Vec<f64> = Vec::new()

          let mut i: i32 = 0;
          while i < 100 {
            let phase: f64 = i as f64 * tau() * freq / sample_rate;
            block.push(sin(phase))
            i = i + 1;
          }

          if block.len() != 100 { ret 1 }
          // One full cycle at 480 Hz / 48 kHz is exactly 100 samples, so
          // sample 0 and (wrapped) sample 50 are sin(0) and sin(pi).
          if fabs(block[0]) > 0.0000001 { ret 2 }
          if fabs(block[50]) > 0.0000001 { ret 3 }
          if block[25] < 0.9999999 { ret 4 }
          if block[75] > -0.9999999 { ret 5 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
