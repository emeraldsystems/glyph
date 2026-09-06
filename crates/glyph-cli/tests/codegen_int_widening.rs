//! GLYPH-73: unsigned values sign-extend when implicitly widened to a wider
//! signed destination.
//!
//! Extension must follow the SOURCE value's own signedness, not the
//! destination type's. A `u32` holding `4294967295` (all bits set) passed to
//! an `i64` parameter must zero-extend to `4294967295`, not sign-extend to
//! `-1`. Signed sources must keep sign-extending exactly as before.
//!
//! Coverage mirrors `codegen_int16.rs`'s approach: IR assertions catch the
//! wrong instruction, runtime assertions catch right-instruction/wrong-value
//! bugs (e.g. a coercion that's a no-op because the width already matched).
//!
//! Boundaries covered: direct call arguments, indirect (closure/Fn) call
//! arguments, `return`, annotated `let` (a plain move), struct literal field
//! initialization, `s.field = v` assignment, and `xs[i] = v` array element
//! assignment. Each is exercised at a value with the high bit set
//! (200/65535/4294967295) so a regression to sign-extension is visible, and
//! signed sources (`-1 as i8`) are checked to confirm they still
//! sign-extend.

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
    build_and_run_stdout(source).0
}

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_stdout(source: &str) -> (i32, String) {
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
    (
        output
            .status
            .code()
            .expect("exited without a code or a signal"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// IR level: the ticket's exact repro, direct call argument
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
#[test]
fn direct_call_unsigned_u32_into_i64_param_emits_zext() {
    let ir = compile_ir_source(
        r#"
        fn f(x: i64) -> i64 { ret x }
        fn g() -> i64 {
          let u: u32 = 4294967295;
          ret f(u)
        }
        "#,
    );
    assert!(
        ir.contains("zext i32") && ir.contains("to i64"),
        "u32 -> i64 call argument must zero-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("sext i32"),
        "u32 -> i64 call argument must not sign-extend:\n{}",
        ir
    );
}

#[cfg(feature = "codegen")]
#[test]
fn direct_call_signed_source_still_emits_sext() {
    let ir = compile_ir_source(
        r#"
        fn f(x: i64) -> i64 { ret x }
        fn g() -> i64 {
          let s: i8 = -1 as i8;
          ret f(s)
        }
        "#,
    );
    assert!(
        ir.contains("sext i8") && ir.contains("to i64"),
        "i8 -> i64 call argument must sign-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("zext i8"),
        "i8 -> i64 call argument must not zero-extend:\n{}",
        ir
    );
}

// ---------------------------------------------------------------------------
// Runtime: the ticket's exact repro and its parameterisation across widths
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn ticket_repro_u32_max_into_i64_param_runtime() {
    let source = r#"
        fn f(x: i64) -> i64 { ret x }

        fn main() -> i32 {
          let u: u32 = 4294967295;
          let r: i64 = f(u);
          if r != 4294967295 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn unsigned_sources_zero_extend_into_wider_signed_call_params_runtime() {
    // u8/u16/u32 into i16/i32/i64, each at a value with the high bit set.
    let source = r#"
        fn take_i16(x: i16) -> i16 { ret x }
        fn take_i32(x: i32) -> i32 { ret x }
        fn take_i64(x: i64) -> i64 { ret x }

        fn main() -> i32 {
          let u8v: u8 = 200;
          if (take_i16(u8v) as i32) != 200 { ret 1 }
          if take_i32(u8v) != 200 { ret 2 }
          if take_i64(u8v) != 200 { ret 3 }

          let u8max: u8 = 255;
          if (take_i16(u8max) as i32) != 255 { ret 4 }
          if take_i32(u8max) != 255 { ret 5 }
          if take_i64(u8max) != 255 { ret 6 }

          let u16v: u16 = 65535;
          if take_i32(u16v) != 65535 { ret 7 }
          if take_i64(u16v) != 65535 { ret 8 }

          let u32v: u32 = 4294967295;
          if take_i64(u32v) != 4294967295 { ret 9 }

          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn signed_sources_still_sign_extend_into_wider_call_params_runtime() {
    let source = r#"
        fn take_i64(x: i64) -> i64 { ret x }

        fn main() -> i32 {
          let s: i8 = -1 as i8;
          if take_i64(s) != -1 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Indirect calls: closures / Fn values (callable.rs)
//
// The resolver requires an EXACT type match between an argument and a
// callable's own declared parameter type at the point of invocation
// (`function(value)`), with no implicit widening at all - confirmed below.
// So `codegen_callable_argument`'s coercion in callable.rs is unreachable
// with a mismatched Glyph source type through any legal Glyph program today;
// see `crates/glyph-backend/src/codegen/tests.rs`'s
// `jit_calls_function_value_zero_extends_unsigned_argument` for a unit test
// that exercises it directly by constructing MIR that bypasses the resolver.
// The fix (derive `signed` from the argument's own type, not the callable's
// parameter type) is still applied in callable.rs on the same principle as
// the direct-call site, both as a correctness fix in its own right and as a
// guard against a future resolver relaxation reintroducing the bug silently.
//
// What IS reachable, and covered here: passing a narrower unsigned value as
// an argument to a regular (direct-call) function that itself takes a
// callable parameter - the widening happens at that direct call site
// (rvalue.rs), before the value ever reaches the indirect invocation.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn calling_a_function_that_takes_a_callable_param_still_zero_extends_the_other_arg_runtime() {
    let source = r#"
        fn take_i64(x: i64) -> i64 { ret x }

        fn apply(function: FnOnce<i64, i64>, value: i64) -> i64 {
          ret function(value)
        }

        fn main() -> i32 {
          let u: u32 = 4294967295;
          let r: i64 = apply(take_i64, u);
          if r != 4294967295 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[test]
fn calling_a_callable_value_directly_requires_an_exact_type_match() {
    // Documents the resolver behavior the two tests above rely on: no
    // implicit widening is permitted when invoking a stored Fn/FnOnce value,
    // even for a lossless, same-signedness widening that a direct call would
    // accept without complaint.
    let source = r#"
        fn take_i64(x: i64) -> i64 { ret x }

        fn apply(function: FnOnce<i64, i64>, value: i32) -> i64 {
          ret function(value)
        }
    "#;
    let out = compile(source);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("expected 'i64'")),
        "expected an exact-type-match error, got: {:?}",
        out.diagnostics
    );
}

// ---------------------------------------------------------------------------
// return
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn return_of_unsigned_source_zero_extends_runtime() {
    let source = r#"
        fn g() -> i64 {
          let u: u8 = 200;
          ret u
        }

        fn main() -> i32 {
          if g() != 200 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn return_of_unsigned_high_bit_source_zero_extends_runtime() {
    let source = r#"
        fn g() -> i64 {
          let u: u8 = 255;
          ret u
        }

        fn main() -> i32 {
          if g() != 255 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Annotated let (a plain move)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn annotated_let_of_unsigned_source_zero_extends_runtime() {
    let source = r#"
        fn main() -> i32 {
          let u: u8 = 200;
          let x: i64 = u;
          if x != 200 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn annotated_let_of_unsigned_source_emits_zext_not_sext() {
    let ir = compile_ir_source(
        r#"
        fn g() -> i64 {
          let u: u8 = 200;
          let x: i64 = u;
          ret x
        }
        "#,
    );
    assert!(
        ir.contains("zext i8") && ir.contains("to i64"),
        "annotated let of an unsigned source must zero-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("sext i8"),
        "annotated let of an unsigned source must not sign-extend:\n{}",
        ir
    );
}

// ---------------------------------------------------------------------------
// Struct literal field store: `S { v: u }`
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_literal_field_store_of_unsigned_source_zero_extends_runtime() {
    let source = r#"
        struct S { v: i64 }

        fn main() -> i32 {
          let u: u8 = 200;
          let s: S = S { v: u };
          if s.v != 200 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// `s.field = v` assignment
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn field_assignment_of_unsigned_source_zero_extends_runtime() {
    let source = r#"
        struct S { v: i64 }

        fn main() -> i32 {
          let mut s: S = S { v: 0 };
          let u: u8 = 200;
          s.v = u;
          if s.v != 200 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// `xs[i] = v` array element assignment
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn array_index_assignment_of_unsigned_source_zero_extends_runtime() {
    let source = r#"
        fn main() -> i32 {
          let mut xs: [i64; 3] = [0, 0, 0];
          let u: u8 = 200;
          xs[1] = u;
          if xs[1] != 200 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Binary ops mixing widths (rvalue.rs coerce_int_binop)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn binary_op_widening_unsigned_operand_zero_extends_runtime() {
    let source = r#"
        fn main() -> i32 {
          let u: u8 = 200;
          let y: i64 = 1;
          let z: i64 = y + u;
          if z != 201 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(feature = "codegen")]
#[test]
fn binary_op_widening_unsigned_operand_emits_zext_not_sext() {
    let ir = compile_ir_source(
        r#"
        fn g() -> i64 {
          let u: u8 = 200;
          let y: i64 = 1;
          ret y + u
        }
        "#,
    );
    assert!(
        ir.contains("zext i8") && ir.contains("to i64"),
        "widening an unsigned operand in a binary op must zero-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("sext i8"),
        "widening an unsigned operand in a binary op must not sign-extend:\n{}",
        ir
    );
}
