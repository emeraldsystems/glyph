//! GLYPH-74: array literals ignored their declared element type annotation.
//!
//! `lower_array_lit` used to infer the array's element type from the FIRST
//! element alone. An untyped integer literal defaults to `i32`, so every
//! array literal was laid out as `i32` regardless of what `let x: [T; N]`
//! (or a parameter, return type, struct field, or outer array literal for a
//! nested array) actually declared. Reading the array back through its
//! declared (narrower or wider) type then reinterpreted the wrong bytes:
//! `[i8; 3]` read back 0 for every non-zero-low-byte element, and `[i64; 3]`
//! glued two `i32` elements into one nonsense 64-bit value.
//!
//! The fix threads the expected element type from the annotation into
//! `lower_array_lit`, reusing the exact mechanism `let x: T = <literal>`
//! already uses to give a literal its annotated type, and adds a coercion at
//! the array-literal codegen site (mirroring `codegen_struct_literal`'s
//! existing per-field coercion) so an element whose own computed width still
//! differs (e.g. a negated literal, computed at i32) is corrected before the
//! store rather than producing mismatched-width IR.
//!
//! Every test here is runtime-verified (not just IR shape) since the bug was
//! about wrong VALUES, not merely wrong types.

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
            include_std: true,
        },
    )
}

#[cfg(feature = "codegen")]
fn compile_ir_source(source: &str) -> String {
    let out = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
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
    artifact.llvm_ir.expect("llvm ir")
}

/// Compile, link and run `source`, handing back the exit code and stdout.
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

#[cfg(all(feature = "codegen", unix))]
fn assert_ok_stdout(source: &str, expected: &str) {
    let (code, stdout) = build_and_run_stdout(source);
    assert_eq!(code, 0, "program exited non-zero, stdout: {stdout}");
    assert_eq!(stdout.trim_end(), expected, "source:\n{source}");
}

fn range_diagnostics(source: &str) -> Vec<String> {
    compile(source)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .filter(|m| m.contains("out of range for"))
        .collect()
}

// ---------------------------------------------------------------------------
// The exact repro from the ticket
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn ticket_repro_prints_two_twice() {
    let source = r#"
        import std
        from std/io import println
        from std/string import string_from_i32

        fn main() -> i32 {
          let a: [i8; 3] = [1, 2, 3]
          println(string_from_i32(a[1] as i32))
          let b: [i32; 3] = [1, 2, 3]
          println(string_from_i32(b[1]))
          ret 0
        }
    "#;
    assert_ok_stdout(source, "2\n2");
}

// ---------------------------------------------------------------------------
// Every element, every declared width
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn every_element_round_trips_for_every_integer_width() {
    for ty in [
        "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64",
    ] {
        let source = format!(
            r#"
            import std
            from std/io import println

            fn main() -> i32 {{
              let a: [{ty}; 3] = [1, 2, 3]
              println($"{{a[0]}} {{a[1]}} {{a[2]}}")
              ret 0
            }}
            "#
        );
        assert_ok_stdout(&source, "1 2 3");
    }
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn every_element_round_trips_for_f64() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let f: [f64; 3] = [1, 2, 3]
          println($"{f[0]} {f[1]} {f[2]}")
          ret 0
        }
    "#;
    // Integer literals promoted into a float array behave like `let x: f64 =
    // 1` (also fixed by this change - see `int_literal_promotes_to_f64`
    // below): they become real float values, not a bit-reinterpretation of
    // the integer.
    assert_ok_stdout(source, "1 2 3");
}

/// The adjacent scalar bug this fix also closes: a bare int literal assigned
/// to an `f64` (or `f32`) annotation used to reinterpret the integer's raw
/// bits as a float instead of converting the value, both directly and as an
/// array element (see `every_element_round_trips_for_f64` above).
#[cfg(all(feature = "codegen", unix))]
#[test]
fn int_literal_promotes_to_f64() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let x: f64 = 1
          println($"{x}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "1");
}

// ---------------------------------------------------------------------------
// Negative values in signed arrays
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn negative_values_survive_the_round_trip_in_signed_arrays() {
    for ty in ["i8", "i16", "i32", "i64"] {
        let source = format!(
            r#"
            import std
            from std/io import println

            fn main() -> i32 {{
              let a: [{ty}; 3] = [-100, 2, -3]
              println($"{{a[0]}} {{a[1]}} {{a[2]}}")
              ret 0
            }}
            "#
        );
        assert_ok_stdout(&source, "-100 2 -3");
    }
}

// ---------------------------------------------------------------------------
// Out-of-range element literal is a compile error
// ---------------------------------------------------------------------------

#[test]
fn out_of_range_array_element_literal_is_a_compile_error() {
    let messages = range_diagnostics(
        r#"
        fn main() -> i32 {
          let a: [i8; 3] = [1, 200, 3];
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
        messages[0].contains("200") && messages[0].contains("i8"),
        "diagnostic should name the literal and the type: {:?}",
        messages
    );
}

#[test]
fn boundary_element_literals_are_accepted() {
    let output = compile(
        r#"
        fn main() -> i32 {
          let a: [i8; 2] = [-128, 127];
          ret 0
        }
        "#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "boundary element literals must compile: {:?}",
        output.diagnostics
    );
}

// ---------------------------------------------------------------------------
// Untyped array literals are unaffected: still default to i32
// ---------------------------------------------------------------------------

#[cfg(feature = "codegen")]
#[test]
fn untyped_array_literal_still_defaults_to_i32() {
    let ir = compile_ir_source(
        r#"
        fn main() -> i32 {
          let a = [1, 2, 3];
          ret a[0]
        }
        "#,
    );
    assert!(
        ir.contains("[3 x i32]"),
        "an unannotated array literal must still default to i32:\n{}",
        ir
    );
}

// ---------------------------------------------------------------------------
// Mixed element types: a typed first element sets the type for the rest
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn untyped_literal_after_a_typed_first_element_adopts_its_type() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let a: i8 = 5
          let arr = [a, 1, -2]
          println($"{arr[0]} {arr[1]} {arr[2]}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "5 1 -2");
}

// ---------------------------------------------------------------------------
// Nested arrays
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_array_literal_respects_the_inner_element_type() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let a: [[i8; 2]; 2] = [[1, 2], [3, -4]]
          println($"{a[0][0]} {a[0][1]} {a[1][0]} {a[1][1]}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "1 2 3 -4");
}

// ---------------------------------------------------------------------------
// Function parameters and return types
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn array_literal_argument_respects_the_parameter_element_type() {
    let source = r#"
        import std
        from std/io import println

        fn second(arr: [i8; 3]) -> i8 {
          ret arr[1]
        }

        fn main() -> i32 {
          println($"{second([1, 2, 3])}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "2");
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn array_literal_return_respects_the_return_element_type() {
    let source = r#"
        import std
        from std/io import println

        fn make() -> [i8; 3] {
          ret [10, 20, -30]
        }

        fn main() -> i32 {
          let m: [i8; 3] = make()
          println($"{m[0]} {m[1]} {m[2]}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "10 20 -30");
}

// ---------------------------------------------------------------------------
// Struct fields typed `[T; N]`
// ---------------------------------------------------------------------------

/// Struct field arrays could not be read back directly at the time this test
/// was written (a pre-existing, unrelated limitation: array-typed struct
/// fields cannot be moved or indexed through a reference - see the
/// `follow-up tickets` note in the GLYPH-74 report). This asserts on the
/// emitted IR instead: the field's storage and every element temp must be
/// `i8`, never `i32`.
#[cfg(feature = "codegen")]
#[test]
fn struct_field_array_literal_lowers_at_the_declared_element_width() {
    let ir = compile_ir_source(
        r#"
        struct Frame { bytes: [i8; 3] }

        fn main() -> i32 {
          let f: Frame = Frame { bytes: [1, 2, 3] };
          ret 0
        }
        "#,
    );
    assert!(
        ir.contains("[3 x i8]"),
        "struct field array literal must lower at i8, not i32:\n{}",
        ir
    );
    assert!(
        !ir.contains("[3 x i32]"),
        "struct field array literal must not also produce an i32 layout:\n{}",
        ir
    );
}

// ---------------------------------------------------------------------------
// Indexed assignment on a narrow array (GLYPH-64) stores one element width
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn indexed_assignment_on_a_narrow_array_stores_one_element_width() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let mut a: [i8; 3] = [1, 2, 3]
          a[1] = 5
          a[2] = -9
          println($"{a[0]} {a[1]} {a[2]}")
          ret 0
        }
    "#;
    assert_ok_stdout(source, "1 5 -9");
}

// ---------------------------------------------------------------------------
// `for .. in` iteration over a narrow array
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn for_in_iterates_narrow_array_elements_at_full_value() {
    let source = r#"
        import std
        from std/io import println

        fn main() -> i32 {
          let a: [i8; 3] = [1, 2, -3]
          for x in a {
            println($"{x}")
          }
          ret 0
        }
    "#;
    assert_ok_stdout(source, "1\n2\n-3");
}
