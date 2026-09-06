//! GLYPH-76: unsigned comparisons and division/remainder always used SIGNED
//! LLVM instructions (`icmp slt`/`sdiv`/`srem`) regardless of the operands'
//! actual Glyph type. An unsigned value with its high bit set (e.g. u32
//! `4000000000`) reads as negative under a signed instruction, so
//! `4000000000u32 < 100u32` evaluated true and `4000000000u32 / 100u32`
//! computed the wrong quotient. This is a distinct bug from GLYPH-73: it
//! affects a SINGLE fixed width with no widening involved at all.
//!
//! Fixed in `codegen/rvalue.rs`'s `Rvalue::Binary` arm: `<`/`<=`/`>`/`>=`
//! select `icmp ult`/`ule`/`ugt`/`uge` and `/`/`%` select `udiv`/`urem` when
//! either genuinely-typed (non-literal) operand is an unsigned integer type;
//! an untyped integer literal (which `mir_value_type` reports as plain
//! `I32`) contributes nothing of its own and defers to the other, typed
//! operand. `Eq`/`Ne`/`Add`/`Sub`/`Mul` are unaffected: two's-complement
//! arithmetic and equality are bit-identical regardless of signedness.
//!
//! No shift operators (`<<`/`>>`) exist in the language yet (`BinaryOp` has
//! no shift variant), so there is no `lshr`/`ashr` selection to make here.
//!
//! Coverage mirrors `codegen_int_widening.rs`: IR assertions catch the wrong
//! instruction, runtime assertions catch right-instruction/wrong-value bugs.
//! Parameterised across u8/u16/u32/u64, each at a value with the high bit
//! set compared/divided against a small value, plus signed controls (which
//! must keep using the signed instructions and truncating-toward-zero
//! division) and one mixed-width unsigned case.

use glyph_frontend::{FrontendOptions, compile_source};

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

    let output = Command::new(&exe_path).output().unwrap();
    if let Some(signal) = output.status.signal() {
        panic!("test binary was killed by signal {signal}");
    }
    output
        .status
        .code()
        .expect("exited without a code or a signal")
}

// ---------------------------------------------------------------------------
// The ticket's exact repro
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn ticket_repro_u32_high_bit_comparison_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u32 = 4000000000;
          let small: u32 = 100;
          if big < small { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Parameterised across u8/u16/u32/u64: <, <=, >, >=, /, % at a high-bit-set
// value against a small one.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn u8_ops_use_unsigned_instructions_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u8 = 200;
          let small: u8 = 50;
          if big < small { ret 1 }
          if big <= small { ret 2 }
          if !(big > small) { ret 3 }
          if !(big >= small) { ret 4 }
          if big / small != 4 { ret 5 }
          if big % small != 0 { ret 6 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn u16_ops_use_unsigned_instructions_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u16 = 60000;
          let small: u16 = 100;
          if big < small { ret 1 }
          if big <= small { ret 2 }
          if !(big > small) { ret 3 }
          if !(big >= small) { ret 4 }
          if big / small != 600 { ret 5 }
          if big % small != 0 { ret 6 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn u32_ops_use_unsigned_instructions_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u32 = 4000000000;
          let small: u32 = 100;
          if big < small { ret 1 }
          if big <= small { ret 2 }
          if !(big > small) { ret 3 }
          if !(big >= small) { ret 4 }
          if big / small != 40000000 { ret 5 }
          if big % small != 0 { ret 6 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn u64_ops_use_unsigned_instructions_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u64 = 18000000000000000000;
          let small: u64 = 100;
          if big < small { ret 1 }
          if big <= small { ret 2 }
          if !(big > small) { ret 3 }
          if !(big >= small) { ret 4 }
          if big / small != 180000000000000000 { ret 5 }
          if big % small != 0 { ret 6 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn u8_ops_emit_unsigned_instructions_not_signed() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u8, b: u8) -> bool {
          let lt: bool = a < b;
          let le: bool = a <= b;
          let gt: bool = a > b;
          let ge: bool = a >= b;
          let d: u8 = a / b;
          let m: u8 = a % b;
          ret lt
        }
        "#,
    );
    for inst in [
        "icmp ult i8",
        "icmp ule i8",
        "icmp ugt i8",
        "icmp uge i8",
        "udiv i8",
        "urem i8",
    ] {
        assert!(ir.contains(inst), "expected `{inst}` in IR:\n{ir}");
    }
    for inst in [
        "icmp slt i8",
        "icmp sle i8",
        "icmp sgt i8",
        "icmp sge i8",
        "sdiv i8",
        "srem i8",
    ] {
        assert!(!ir.contains(inst), "did not expect `{inst}` in IR:\n{ir}");
    }
}

#[cfg(feature = "codegen")]
#[test]
fn u16_ops_emit_unsigned_instructions_not_signed() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u16, b: u16) -> bool {
          let lt: bool = a < b;
          let le: bool = a <= b;
          let gt: bool = a > b;
          let ge: bool = a >= b;
          let d: u16 = a / b;
          let m: u16 = a % b;
          ret lt
        }
        "#,
    );
    for inst in [
        "icmp ult i16",
        "icmp ule i16",
        "icmp ugt i16",
        "icmp uge i16",
        "udiv i16",
        "urem i16",
    ] {
        assert!(ir.contains(inst), "expected `{inst}` in IR:\n{ir}");
    }
    for inst in [
        "icmp slt i16",
        "icmp sle i16",
        "icmp sgt i16",
        "icmp sge i16",
        "sdiv i16",
        "srem i16",
    ] {
        assert!(!ir.contains(inst), "did not expect `{inst}` in IR:\n{ir}");
    }
}

#[cfg(feature = "codegen")]
#[test]
fn u32_ops_emit_unsigned_instructions_not_signed() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u32, b: u32) -> bool {
          let lt: bool = a < b;
          let le: bool = a <= b;
          let gt: bool = a > b;
          let ge: bool = a >= b;
          let d: u32 = a / b;
          let m: u32 = a % b;
          ret lt
        }
        "#,
    );
    for inst in [
        "icmp ult i32",
        "icmp ule i32",
        "icmp ugt i32",
        "icmp uge i32",
        "udiv i32",
        "urem i32",
    ] {
        assert!(ir.contains(inst), "expected `{inst}` in IR:\n{ir}");
    }
    for inst in [
        "icmp slt i32",
        "icmp sle i32",
        "icmp sgt i32",
        "icmp sge i32",
        "sdiv i32",
        "srem i32",
    ] {
        assert!(!ir.contains(inst), "did not expect `{inst}` in IR:\n{ir}");
    }
}

#[cfg(feature = "codegen")]
#[test]
fn u64_ops_emit_unsigned_instructions_not_signed() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u64, b: u64) -> bool {
          let lt: bool = a < b;
          let le: bool = a <= b;
          let gt: bool = a > b;
          let ge: bool = a >= b;
          let d: u64 = a / b;
          let m: u64 = a % b;
          ret lt
        }
        "#,
    );
    for inst in [
        "icmp ult i64",
        "icmp ule i64",
        "icmp ugt i64",
        "icmp uge i64",
        "udiv i64",
        "urem i64",
    ] {
        assert!(ir.contains(inst), "expected `{inst}` in IR:\n{ir}");
    }
    for inst in [
        "icmp slt i64",
        "icmp sle i64",
        "icmp sgt i64",
        "icmp sge i64",
        "sdiv i64",
        "srem i64",
    ] {
        assert!(!ir.contains(inst), "did not expect `{inst}` in IR:\n{ir}");
    }
}

// ---------------------------------------------------------------------------
// Signed controls: must keep using signed instructions and truncating-
// toward-zero division/remainder.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn signed_comparison_and_division_semantics_runtime() {
    let source = r#"
        fn main() -> i32 {
          let neg: i32 = 0 - 1;
          if !(neg < 1) { ret 1 }
          let q: i32 = (0 - 7) / 2;
          if q != (0 - 3) { ret 2 }
          let r: i32 = (0 - 7) % 2;
          if r != (0 - 1) { ret 3 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn signed_operands_still_emit_signed_instructions() {
    let ir = compile_ir_source(
        r#"
        fn g(a: i32, b: i32) -> bool {
          let lt: bool = a < b;
          let d: i32 = a / b;
          let m: i32 = a % b;
          ret lt
        }
        "#,
    );
    assert!(
        ir.contains("icmp slt i32"),
        "expected `icmp slt i32`:\n{ir}"
    );
    assert!(ir.contains("sdiv i32"), "expected `sdiv i32`:\n{ir}");
    assert!(ir.contains("srem i32"), "expected `srem i32`:\n{ir}");
    assert!(
        !ir.contains("icmp ult i32") && !ir.contains("udiv i32") && !ir.contains("urem i32"),
        "did not expect unsigned instructions for signed operands:\n{ir}"
    );
}

// ---------------------------------------------------------------------------
// Mixed-width unsigned operands: after GLYPH-73's zero-extension, the
// comparison must still select the unsigned predicate.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn mixed_width_unsigned_operands_use_unsigned_comparison_runtime() {
    let source = r#"
        fn main() -> i32 {
          let a: u32 = 4000000000;
          let b: u8 = 100;
          if !(a > b) { ret 1 }
          if a < b { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn mixed_width_unsigned_operands_emit_unsigned_comparison() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u32, b: u8) -> bool {
          ret a > b
        }
        "#,
    );
    assert!(
        ir.contains("icmp ugt i32"),
        "mixed-width unsigned comparison must use `icmp ugt`:\n{ir}"
    );
    assert!(
        !ir.contains("icmp sgt i32"),
        "mixed-width unsigned comparison must not use `icmp sgt`:\n{ir}"
    );
}

// ---------------------------------------------------------------------------
// An untyped integer literal compared against an unsigned local defers to
// the local's signedness.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn literal_against_unsigned_local_uses_unsigned_comparison_runtime() {
    let source = r#"
        fn main() -> i32 {
          let big: u32 = 4000000000;
          if big < 100 { ret 1 }
          if 100 > big { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn literal_against_unsigned_local_emits_unsigned_comparison() {
    let ir = compile_ir_source(
        r#"
        fn g(a: u32) -> bool {
          ret a < 100
        }
        "#,
    );
    assert!(
        ir.contains("icmp ult i32"),
        "an untyped literal against an unsigned local must compare unsigned:\n{ir}"
    );
    assert!(
        !ir.contains("icmp slt i32"),
        "an untyped literal against an unsigned local must not compare signed:\n{ir}"
    );
}
