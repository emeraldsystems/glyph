//! End-to-end coverage for f32/f64 support: literals, arithmetic, comparisons,
//! casts through function calls, printing, and containers.
//!
//! Before floats were wired through MIR (Rvalue::ConstFloat / MirValue::Float),
//! `let x: f64 = 1.5` silently lowered to a Nop (leaving x uninitialized) and
//! any float arithmetic failed LLVM verification with "Integer arithmetic
//! operators only work with integral types!".

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
fn build_binary(source: &str, temp: &TempDir) -> Option<std::path::PathBuf> {
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

    Some(exe_path)
}

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    let temp = TempDir::new().unwrap();
    let Some(exe_path) = build_binary(source, &temp) else {
        return 0;
    };

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
fn build_and_run_stdout(source: &str) -> String {
    let temp = TempDir::new().unwrap();
    let Some(exe_path) = build_binary(source, &temp) else {
        return String::new();
    };

    let output = Command::new(&exe_path).output().unwrap();
    assert!(
        output.status.success(),
        "binary exited with {:?}",
        output.status
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn float_literal_lowers_to_const_float_not_nop() {
    let source = r#"import std

fn main() -> i32 {
  let x: f64 = 1.5;
  let y: f64 = x + 2.5;
  if y > 3.0 { ret 1 }
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
        mir_text.contains("ConstFloat"),
        "float literals must lower to ConstFloat, not vanish into Nop"
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn float_arithmetic_and_comparisons_runtime() {
    let source = r#"
        fn main() -> i32 {
          let x: f64 = 1.5;
          let y: f64 = 2.5;
          let z: f64 = x + y;
          if z != 4.0 { ret 1 }

          let d: f64 = 7.5 / 2.5;
          if d != 3.0 { ret 2 }

          let r: f64 = 7.5 % 2.0;
          if r != 1.5 { ret 3 }

          if z <= 3.9 { ret 4 }
          if z >= 4.1 { ret 5 }
          if z < 4.0 { ret 6 }
          if z > 4.0 { ret 7 }

          let neg: f64 = -x;
          if neg != -1.5 { ret 8 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn f32_annotated_arithmetic_runtime() {
    let source = r#"
        fn main() -> i32 {
          let a: f32 = 1.5;
          let b: f32 = 0.25;
          let p: f32 = a * b;
          if p != 0.375 { ret 1 }

          let s: f32 = a - b;
          if s != 1.25 { ret 2 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn mixed_int_float_promotion_runtime() {
    let source = r#"
        fn main() -> i32 {
          let n: i32 = 3;
          let x: f64 = 1.5;
          let m: f64 = n * x;
          if m != 4.5 { ret 1 }

          let u: u32 = 4;
          let q: f64 = u * 0.25;
          if q != 1.0 { ret 2 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn float_function_params_and_return_runtime() {
    let source = r#"
        fn scale(x: f64, k: f64) -> f64 {
          ret x * k
        }

        fn mix(a: f64, b: f64, t: f64) -> f64 {
          ret a * (1.0 - t) + b * t
        }

        fn main() -> i32 {
          let s: f64 = scale(2.5, 4.0);
          if s != 10.0 { ret 1 }

          let m: f64 = mix(0.0, 8.0, 0.25);
          if m != 2.0 { ret 2 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_f64_and_struct_float_fields_runtime() {
    let source = r#"
        from std/vec import Vec

        struct Osc {
          freq: f64,
          gain: f64,
        }

        fn main() -> i32 {
          let mut buf: Vec<f64> = Vec::new()
          buf.push(1.5)
          buf.push(2.5)
          buf.push(-0.5)
          if buf.len() != 3 { ret 1 }
          let a: f64 = buf[0]
          let b: f64 = buf[1]
          let c: f64 = buf[2]
          let sum: f64 = a + b + c
          if sum != 3.5 { ret 2 }

          let o = Osc { freq: 440.0, gain: 0.5 }
          let scaled: f64 = o.freq * o.gain
          if scaled != 220.0 { ret 3 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn float_print_shortest_roundtrip_output() {
    let source = r#"
        from std import println

        fn main() -> i32 {
          let z: f64 = 1.5 + 2.5;
          let h: f64 = 0.1;
          let f: f32 = 0.25;
          let big: f64 = 220.0;
          let neg: f64 = -3.25;
          println($"z={z}")
          println($"h={h}")
          println($"f={f}")
          println($"big={big}")
          println($"neg={neg}")
          ret 0
        }
    "#;

    let stdout = build_and_run_stdout(source);
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }
    assert_eq!(stdout, "z=4\nh=0.1\nf=0.25\nbig=220\nneg=-3.25\n");
}

// Regression: scalar and string print segments used to lower to calls of
// nonexistent fmt_* symbols and died in codegen with "unknown function";
// they now call the glyph_fmt_write_* externs in the C runtime.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn int_bool_string_print_links_and_runs() {
    let source = r#"
        from std import println

        fn main() -> i32 {
          let x: i32 = 42;
          // Note: u64 values above i64::MAX cannot be written as literals yet —
          // the AST literal is i64 (see the "MIR lowering total" backlog task).
          let big: u64 = 9007199254740993;
          let flag: bool = true;
          let s = String::from_str("owned")
          let t: str = "borrowed"
          println($"x={x}")
          println($"big={big}")
          println($"flag={flag}")
          println(s)
          println(t)
          ret 0
        }
    "#;

    let stdout = build_and_run_stdout(source);
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }
    assert_eq!(
        stdout,
        "x=42\nbig=9007199254740993\nflag=true\nowned\nborrowed\n"
    );
}

// libm lives inside libSystem on macOS, so extern math functions link without
// extra flags there. On Linux this needs -lm (see the std/math backlog task).
#[cfg(all(feature = "codegen", target_os = "macos"))]
#[test]
fn extern_libm_sin_sqrt_runtime() {
    let source = r#"
        from std import println

        extern "C" fn sin(x: f64) -> f64;
        extern "C" fn sqrt(x: f64) -> f64;

        fn main() -> i32 {
          let zero: f64 = sin(0.0);
          if zero != 0.0 { ret 1 }
          let r: f64 = sqrt(16.0);
          if r != 4.0 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
