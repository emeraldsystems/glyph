//! `i16` / `u16` coverage: LLVM type mapping, the three conversions that are
//! easy to confuse (sign-extend, zero-extend, truncate), the documented
//! wrapping arithmetic rule at both boundaries, `Vec<i16>`, out-of-range
//! literals, and an `extern "C"` function taking and returning `short`.
//!
//! Extension is asserted twice on purpose: once on the emitted IR, which
//! catches the wrong instruction, and once on the runtime value, which catches
//! right instruction / wrong semantics. A `u16 as i32` that sign-extends still
//! produces 200 for 200; it only misbehaves above 0x7FFF.

use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(feature = "codegen")]
use std::fs;
#[cfg(feature = "codegen")]
use std::path::Path;

#[cfg(feature = "codegen")]
use glyph_backend::llvm::LlvmBackend;
#[cfg(feature = "codegen")]
use glyph_backend::{Backend, CodegenOptions, EmitKind};

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

#[cfg(feature = "codegen")]
fn load_fixture(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codegen");
    let path = root.join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture read {}: {}", path.display(), e))
}

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    )
}

#[cfg(feature = "codegen")]
fn compile_ir_source(source: &str) -> String {
    let out = compile(source);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let backend = LlvmBackend::default();
    let artifact = backend
        .emit(
            &out.mir,
            &CodegenOptions {
                emit: EmitKind::LlvmIr,
                ..Default::default()
            },
        )
        .expect("backend emit");
    let ir = artifact.llvm_ir.expect("llvm ir");
    if std::env::var("GLYPH_DEBUG_IR").is_ok() {
        eprintln!("{}", ir);
    }
    ir
}

#[cfg(feature = "codegen")]
fn compile_ir(fixture: &str) -> String {
    compile_ir_source(&load_fixture(fixture))
}

/// Compile, link and run `source`, reporting the process exit code. Death by
/// signal panics rather than being reported as a clean exit.
#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    build_and_run_with_objects(source, Vec::new()).0
}

/// As above, but also hands back stdout so a test can assert on what the
/// program printed rather than only on how it exited.
#[cfg(all(feature = "codegen", unix))]
fn build_and_run_stdout(source: &str) -> (i32, String) {
    build_and_run_with_objects(source, Vec::new())
}

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_with_objects(
    source: &str,
    extra_objects: Vec<std::path::PathBuf>,
) -> (i32, String) {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return (0, String::new());
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
        return (0, String::new());
    }
    ctx.emit_object_file(&obj_path).unwrap();

    let mut object_files = vec![obj_path];
    object_files.extend(extra_objects);

    let linker = Linker::new();
    let opts = LinkerOptions {
        output_path: exe_path.clone(),
        object_files,
        link_libs: Vec::new(),
        link_search_paths: Vec::new(),
        runtime_lib_path: Linker::get_runtime_lib_path(),
    };
    linker.link(&opts).unwrap();

    let output = Command::new(&exe_path).output().unwrap();
    if let Some(signal) = output.status.signal() {
        // A crash is a failure, never a reason to treat the run as skipped.
        panic!("test binary was killed by signal {signal}");
    }
    (
        output
            .status
            .code()
            .expect("exited without a code or a signal"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// IR level
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
#[test]
fn sixteen_bit_scalars_map_to_llvm_i16() {
    let ir = compile_ir("int16_casts.glyph");
    assert!(
        ir.contains("define i32 @widen_signed(i16"),
        "i16 parameter should lower to LLVM i16:\n{}",
        ir
    );
    assert!(
        ir.contains("define i32 @widen_unsigned(i16"),
        "u16 parameter should lower to LLVM i16:\n{}",
        ir
    );
    assert!(
        ir.contains("define i16 @narrow(i32"),
        "an i16 return should lower to LLVM i16:\n{}",
        ir
    );
}

#[cfg(feature = "codegen")]
#[test]
fn i16_to_i32_emits_sext() {
    let ir = compile_ir_source(
        r#"
        fn widen(s: i16) -> i32 {
          ret s as i32
        }
        "#,
    );
    assert!(
        ir.contains("sext i16") && ir.contains("to i32"),
        "i16 -> i32 must sign-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("zext i16"),
        "i16 -> i32 must not zero-extend:\n{}",
        ir
    );
}

#[cfg(feature = "codegen")]
#[test]
fn u16_to_i32_emits_zext() {
    let ir = compile_ir_source(
        r#"
        fn widen(u: u16) -> i32 {
          ret u as i32
        }
        "#,
    );
    assert!(
        ir.contains("zext i16") && ir.contains("to i32"),
        "u16 -> i32 must zero-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("sext i16"),
        "u16 -> i32 must not sign-extend:\n{}",
        ir
    );
}

#[cfg(feature = "codegen")]
#[test]
fn i32_to_i16_emits_trunc() {
    let ir = compile_ir_source(
        r#"
        fn narrow(n: i32) -> i16 {
          ret n as i16
        }
        "#,
    );
    assert!(
        ir.contains("trunc i32") && ir.contains("to i16"),
        "i32 -> i16 must truncate:\n{}",
        ir
    );
}

// ---------------------------------------------------------------------------
// Runtime semantics
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn boundary_literals_round_trip_through_a_function_runtime() {
    let source = r#"
        fn ident_i16(x: i16) -> i16 { ret x }
        fn ident_u16(x: u16) -> u16 { ret x }

        fn main() -> i32 {
          let lo: i16 = -32768;
          if (ident_i16(lo) as i32) != -32768 { ret 1 }

          let hi: i16 = 32767;
          if (ident_i16(hi) as i32) != 32767 { ret 2 }

          let zero: u16 = 0;
          if (ident_u16(zero) as i32) != 0 { ret 3 }

          let top: u16 = 65535;
          if (ident_u16(top) as i32) != 65535 { ret 4 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn boundary_values_print_runtime() {
    // `print` routes i16 through the i32 writer and u16 through the u32
    // writer, so the extension applied on the way in is visible in the
    // formatted text: sign-extending the u16 would print 4294967295, and
    // zero-extending the i16 would print 32768.
    let source = r#"
        import std

        fn main() -> i32 {
          let lo: i16 = -32768;
          let hi: i16 = 32767;
          let bottom: u16 = 0;
          let top: u16 = 65535;
          std::println($"{lo} {hi} {bottom} {top}")
          ret 0
        }
    "#;

    let (code, stdout) = build_and_run_stdout(source);
    assert_eq!(code, 0);
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }
    assert_eq!(stdout.trim(), "-32768 32767 0 65535");
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn sign_and_zero_extension_semantics_runtime() {
    let source = r#"
        fn main() -> i32 {
          // i16 sign-extends
          let neg: i16 = -1 as i16;
          if (neg as i32) != -1 { ret 1 }
          if (neg as i64) != -1 { ret 2 }

          // u16 zero-extends: the same bit pattern, read unsigned
          let top: u16 = 65535 as u16;
          if (top as i32) != 65535 { ret 3 }
          if (top as i64) != 65535 { ret 4 }

          // an i16 holding the same bits reads back as -1
          let same_bits: i16 = 65535 as i16;
          if (same_bits as i32) != -1 { ret 5 }

          // below 0x8000 the two extensions agree; this must keep working
          let small_s: i16 = 200;
          let small_u: u16 = 200;
          if (small_s as i32) != 200 { ret 6 }
          if (small_u as i32) != 200 { ret 7 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn truncation_semantics_runtime() {
    let source = r#"
        fn main() -> i32 {
          // 70000 = 0x11170; the low 16 bits are 0x1170 = 4464
          let wide: i32 = 70000;
          if ((wide as i16) as i32) != 4464 { ret 1 }

          // 40000 = 0x9C40, which is negative read as i16
          let big: i32 = 40000;
          if ((big as i16) as i32) != -25536 { ret 2 }
          if ((big as u16) as i32) != 40000 { ret 3 }

          // truncating a negative source keeps the low bits
          let neg: i32 = -1;
          if ((neg as u16) as i32) != 65535 { ret 4 }

          let wide64: i64 = 4294967298;
          if ((wide64 as u16) as i32) != 2 { ret 5 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// The documented promotion rule: same-width operands stay at that width and
/// wrap two's-complement. If Glyph ever adopts C-style promotion to i32, these
/// four assertions are the ones that change.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn arithmetic_stays_sixteen_bit_and_wraps_runtime() {
    let source = r#"
        fn main() -> i32 {
          let hi: i16 = 32767;
          let one: i16 = 1;
          let over: i16 = hi + one;
          if (over as i32) != -32768 { ret 1 }

          let lo: i16 = -32768;
          let under: i16 = lo - one;
          if (under as i32) != 32767 { ret 2 }

          let utop: u16 = 65535;
          let uone: u16 = 1;
          let uover: u16 = utop + uone;
          if (uover as i32) != 0 { ret 3 }

          let uzero: u16 = 0;
          let uunder: u16 = uzero - uone;
          if (uunder as i32) != 65535 { ret 4 }

          // widening first is the deliberate opt-out from wrapping
          let widened: i32 = (hi as i32) + (one as i32);
          if widened != 32768 { ret 5 }

          // mixed widths evaluate at the wider type
          let mixed: i32 = 32767;
          let sum: i32 = mixed + (one as i32);
          if sum != 32768 { ret 6 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_of_i16_builds_indexes_and_passes_by_reference_runtime() {
    let source = r#"
        fn sum_samples(v: &Vec<i16>) -> i32 {
          let mut total: i32 = 0;
          let mut i: usize = 0;
          while i < v.len() {
            let s: i16 = v[i];
            total = total + (s as i32);
            i = i + 1;
          }
          ret total
        }

        fn main() -> i32 {
          let mut v: Vec<i16> = Vec::new();
          v.push(-32768 as i16);
          v.push(32767 as i16);
          v.push(-1 as i16);

          if v.len() != 3 { ret 1 }
          if (v[0] as i32) != -32768 { ret 2 }
          if (v[1] as i32) != 32767 { ret 3 }
          if (v[2] as i32) != -1 { ret 4 }

          // -32768 + 32767 + -1 == -2, and only sign extension gets there
          if sum_samples(&v) != -2 { ret 5 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_of_u16_zero_extends_on_widening_runtime() {
    let source = r#"
        fn sum_samples(v: &Vec<u16>) -> i32 {
          let mut total: i32 = 0;
          let mut i: usize = 0;
          while i < v.len() {
            let s: u16 = v[i];
            total = total + (s as i32);
            i = i + 1;
          }
          ret total
        }

        fn main() -> i32 {
          let mut v: Vec<u16> = Vec::new();
          v.push(65535 as u16);
          v.push(1 as u16);

          if sum_samples(&v) != 65536 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// The motivating case: a little-endian 16-bit sample pulled out of a byte
/// buffer without hand-rolled two's-complement arithmetic.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn little_endian_sample_decode_runtime() {
    let source = r#"
        fn decode(b0: u8, b1: u8) -> i16 {
          let lo: u16 = b0 as u16;
          let hi: u16 = b1 as u16;
          let raw: u16 = lo + (hi * 256);
          ret raw as i16
        }

        fn main() -> i32 {
          // 0x8000 -> -32768
          if (decode(0 as u8, 128 as u8) as i32) != -32768 { ret 1 }
          // 0xFFFF -> -1
          if (decode(255 as u8, 255 as u8) as i32) != -1 { ret 2 }
          // 0x7FFF -> 32767
          if (decode(255 as u8, 127 as u8) as i32) != 32767 { ret 3 }
          // 0x0001 -> 1
          if (decode(1 as u8, 0 as u8) as i32) != 1 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Out-of-range literals
// ---------------------------------------------------------------------------

fn range_diagnostics(source: &str) -> Vec<String> {
    compile(source)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .filter(|m| m.contains("out of range for"))
        .collect()
}

#[test]
fn out_of_range_i16_literal_is_a_compile_error() {
    let messages = range_diagnostics(
        r#"
        fn main() -> i32 {
          let x: i16 = 40000;
          ret 0
        }
        "#,
    );
    assert_eq!(
        messages.len(),
        1,
        "expected exactly one range diagnostic, got {:?}",
        messages
    );
    assert!(
        messages[0].contains("40000") && messages[0].contains("i16"),
        "diagnostic should name the literal and the type: {:?}",
        messages
    );
}

#[test]
fn out_of_range_u16_literals_are_compile_errors() {
    for (literal, why) in [("70000", "above u16::MAX"), ("-1", "below zero")] {
        let source = format!(
            r#"
            fn main() -> i32 {{
              let x: u16 = {};
              ret 0
            }}
            "#,
            literal
        );
        let messages = range_diagnostics(&source);
        assert_eq!(
            messages.len(),
            1,
            "u16 literal {} ({}) should be rejected once, got {:?}",
            literal,
            why,
            messages
        );
    }
}

#[test]
fn out_of_range_literal_is_rejected_at_a_call_and_return_boundary() {
    let messages = range_diagnostics(
        r#"
        fn takes(x: i16) -> i16 { ret x }

        fn produces() -> u16 {
          ret 100000
        }

        fn main() -> i32 {
          takes(40000);
          ret 0
        }
        "#,
    );
    assert_eq!(
        messages.len(),
        2,
        "both the argument and the return literal should be rejected: {:?}",
        messages
    );
}

#[test]
fn out_of_range_struct_field_literal_is_a_compile_error() {
    let messages = range_diagnostics(
        r#"
        struct Frame { left: i16, flags: u16 }

        fn main() -> i32 {
          let f: Frame = Frame { left: 40000, flags: 70000 };
          ret 0
        }
        "#,
    );
    assert_eq!(
        messages.len(),
        2,
        "both out-of-range struct fields should be rejected: {:?}",
        messages
    );
}

#[test]
fn in_range_struct_field_literals_are_accepted() {
    let output = compile(
        r#"
        struct Frame { left: i16, flags: u16 }

        fn main() -> i32 {
          let f: Frame = Frame { left: -32768, flags: 65535 };
          ret (f.left as i32) + (f.flags as i32)
        }
        "#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "boundary struct fields must compile: {:?}",
        output.diagnostics
    );
}

/// Struct fields carrying 16-bit scalars survive construction and read-back.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_fields_hold_sixteen_bit_scalars_runtime() {
    let source = r#"
        struct Frame { left: i16, right: i16, flags: u16 }

        fn main() -> i32 {
          let f: Frame = Frame { left: -32768, right: 32767, flags: 65535 };
          if (f.left as i32) != -32768 { ret 1 }
          if (f.right as i32) != 32767 { ret 2 }
          if (f.flags as i32) != 65535 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[test]
fn boundary_literals_are_accepted() {
    let output = compile(
        r#"
        fn main() -> i32 {
          let a: i16 = -32768;
          let b: i16 = 32767;
          let c: u16 = 0;
          let d: u16 = 65535;
          ret (a as i32) + (b as i32) + (c as i32) + (d as i32)
        }
        "#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "boundary literals must compile: {:?}",
        output.diagnostics
    );
}

// ---------------------------------------------------------------------------
// extern "C"
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
#[test]
fn extern_c_short_declares_i16() {
    let ir = compile_ir_source(
        r#"
        extern "C" fn glyph_test_double_short(x: i16) -> i16;

        fn call_it(v: i16) -> i16 {
          ret glyph_test_double_short(v)
        }
        "#,
    );
    assert!(
        ir.contains("declare i16 @glyph_test_double_short(i16)"),
        "extern taking and returning short should declare i16:\n{}",
        ir
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn extern_c_short_round_trips_runtime() {
    let temp = TempDir::new().unwrap();
    let c_path = temp.path().join("shorts.c");
    let c_obj = temp.path().join("shorts.o");

    fs::write(
        &c_path,
        r#"
short glyph_test_double_short(short x) { return (short)(x * 2); }
short glyph_test_identity_short(short x) { return x; }
unsigned short glyph_test_identity_ushort(unsigned short x) { return x; }
"#,
    )
    .unwrap();

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&cc)
        .arg("-c")
        .arg(&c_path)
        .arg("-o")
        .arg(&c_obj)
        .status()
        .unwrap();
    assert!(status.success(), "compiling the C helper failed");

    let source = r#"
        extern "C" fn glyph_test_double_short(x: i16) -> i16;
        extern "C" fn glyph_test_identity_short(x: i16) -> i16;
        extern "C" fn glyph_test_identity_ushort(x: u16) -> u16;

        fn main() -> i32 {
          let v: i16 = 1000;
          if (glyph_test_double_short(v) as i32) != 2000 { ret 1 }

          // C wraps the short multiply the same way Glyph does
          let hi: i16 = 20000;
          if (glyph_test_double_short(hi) as i32) != -25536 { ret 2 }

          let lo: i16 = -32768;
          if (glyph_test_identity_short(lo) as i32) != -32768 { ret 3 }

          let top: u16 = 65535;
          if (glyph_test_identity_ushort(top) as i32) != 65535 { ret 4 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_with_objects(source, vec![c_obj]).0, 0);
}
