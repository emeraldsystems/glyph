//! `expr as T` numeric cast coverage: int<->float, int width changes,
//! f32<->f64, bool/char sources, and Rust-style semantics (extension by
//! source signedness, float->int truncation toward zero).

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

#[test]
fn cast_lowers_to_mir_cast() {
    let source = r#"import std

fn main() -> i32 {
  let n: i32 = 7;
  let x: f64 = n as f64;
  if x > 6.5 { ret 1 }
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mir_text = format!("{:?}", output.mir);
    assert!(
        mir_text.contains("Cast"),
        "MIR should contain a Cast rvalue"
    );
}

#[test]
fn cast_to_non_numeric_type_is_rejected() {
    let source = r#"import std

fn main() -> i32 {
  let n: i32 = 7;
  let s: str = n as str;
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cast target must be a numeric type")),
        "expected a cast-target diagnostic, got: {:?}",
        output.diagnostics
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn int_float_roundtrips_runtime() {
    let source = r#"
        fn main() -> i32 {
          let n: i32 = 7;
          let x: f64 = n as f64;
          if x != 7.0 { ret 1 }

          let y: f64 = 3.99;
          if y as i32 != 3 { ret 2 }

          let neg: f64 = -3.99;
          if neg as i32 != -3 { ret 3 }

          let u: u32 = 4000000000;
          let uf: f64 = u as f64;
          if uf != 4000000000.0 { ret 4 }

          let half: f32 = 0.5;
          let wide: f64 = half as f64;
          if wide != 0.5 { ret 5 }
          let narrow: f32 = wide as f32;
          if narrow != 0.5 { ret 6 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn int_width_and_sign_semantics_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: i64 = 300;
          let b: u8 = big as u8;
          if b != 44 { ret 1 }

          let sb: i8 = -1 as i8;
          let widened: u32 = sb as u32;
          if widened != 4294967295 { ret 2 }

          let ub: u8 = 200;
          let zext: i32 = ub as i32;
          if zext != 200 { ret 3 }

          let phase: i64 = 4294967298;
          let low: u32 = phase as u32;
          if low != 2 { ret 4 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bool_and_char_sources_runtime() {
    let source = r#"
        fn main() -> i32 {
          let flag: bool = true;
          if flag as i32 != 1 { ret 1 }

          let c: char = 'A';
          if c as i32 != 65 { ret 2 }

          let midi: u8 = c as u8;
          if midi != 65 { ret 3 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// The DSP shape casts unlock: integer phase accumulator normalized to float.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn phase_accumulator_normalization_runtime() {
    let source = r#"
        fn main() -> i32 {
          let sample_rate: i64 = 48000;
          let mut phase: i64 = 0;
          let mut i: i32 = 0;
          while i < 480 {
            phase = phase + 100;
            i = i + 1;
          }
          let norm: f64 = phase as f64 / sample_rate as f64;
          if norm != 1.0 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Regression: `let c: char = 'A'` used to lower to a silent Nop in the
// expression path (only interpolation's value path handled Char literals),
// leaving c uninitialized.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn char_literal_let_binding_initializes() {
    let source = r#"
        fn main() -> i32 {
          let c: char = 'A';
          ret c as i32
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 65);
}
