//! GLYPH-72: `Vec<u8>` (and any other narrower-than-i32 element type)
//! corrupted the heap once growth passed 32 elements, roughly 10 runs in 12.
//!
//! Root cause: `codegen_value`/`codegen_value_owned` always materialize an
//! untyped integer literal as LLVM `i32` (see `MirValue::Int` in
//! `codegen/rvalue.rs`), with no knowledge of the destination element type.
//! `Vec::push` codegen stored that value straight through the element
//! pointer with no width coercion. LLVM's opaque pointers carry no
//! pointee-type information, so `store i32 0, ptr %p` is valid IR even when
//! `%p` was computed as `getelementptr i8, ...` -- it just silently
//! overruns the 1-byte slot by 3 bytes per push. That lands in unused
//! allocator slack for small buffers (no visible symptom below 32 elements)
//! and in live heap metadata once the buffer is big enough, which is why
//! the bug was size-dependent and non-deterministic (SIGKILL/SIGTRAP)
//! rather than a clean crash every time.
//!
//! Fixed in `codegen/vec.rs` via `coerce_vec_elem_for_store`, called from
//! both `codegen_vec_push` and `codegen_vec_push_no_grow` before every
//! element store. It mirrors the width coercion indexed assignment
//! (`xs[i] = v`, GLYPH-64) already applies in `functions.rs`.
//!
//! Test strategy: an IR-level test asserts directly on the store
//! instruction's width, so the regression is caught deterministically
//! rather than relying on catching an intermittent crash. Runtime tests
//! then cover the acceptance criteria: the exact repro run 100 times, a
//! million-element `Vec<u8>` with full-buffer verification, and the same
//! push/read-back shape parameterised over u8, i8, u32, i32, u64 and f64.

#![cfg(feature = "codegen")]

use glyph_backend::llvm::LlvmBackend;
use glyph_backend::{Backend, CodegenOptions, EmitKind};
use glyph_frontend::{FrontendOptions, compile_source};

fn compile_ir_source(source: &str) -> String {
    let out = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
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

/// Finds the line that stores through `ptr_name`, so the assertion is
/// robust to whatever the stored operand happens to be named.
fn find_store_line<'a>(ir: &'a str, ptr_name: &str) -> &'a str {
    ir.lines()
        .find(|l| l.contains("store") && l.contains(ptr_name))
        .unwrap_or_else(|| panic!("no store to {} found in IR:\n{}", ptr_name, ir))
}

// ---------------------------------------------------------------------------
// IR level: deterministic regression test.
//
// A non-deterministic runtime bug can hide behind a lucky run (the ticket
// measured roughly 10 crashes in 12). This asserts directly on the
// instruction that was wrong, so a regression fails every time, not most
// times.
// ---------------------------------------------------------------------------

#[test]
fn vec_u8_push_stores_i8_not_i32_through_the_element_pointer() {
    let ir = compile_ir_source(
        r#"
        fn main() -> i32 {
          let mut buf: Vec<u8> = Vec::new()
          buf.push(0)
          ret 0
        }
        "#,
    );
    let line = find_store_line(&ir, "%vec.push.dest");
    assert!(
        line.trim_start().starts_with("store i8 "),
        "Vec<u8>.push(0) must store a single byte through the element \
         pointer -- a wider store here overruns the 1-byte slot (GLYPH-72 \
         heap corruption). Store instruction was: {}\nfull IR:\n{}",
        line,
        ir
    );
}

#[test]
fn vec_i8_push_stores_i8_not_i32_through_the_element_pointer() {
    let ir = compile_ir_source(
        r#"
        fn main() -> i32 {
          let mut buf: Vec<i8> = Vec::new()
          buf.push(0)
          ret 0
        }
        "#,
    );
    let line = find_store_line(&ir, "%vec.push.dest");
    assert!(
        line.trim_start().starts_with("store i8 "),
        "Vec<i8>.push(0) must store a single byte: {}\nfull IR:\n{}",
        line,
        ir
    );
}

#[test]
fn vec_u16_push_stores_i16_not_i32_through_the_element_pointer() {
    let ir = compile_ir_source(
        r#"
        fn main() -> i32 {
          let mut buf: Vec<u16> = Vec::new()
          buf.push(0)
          ret 0
        }
        "#,
    );
    let line = find_store_line(&ir, "%vec.push.dest");
    assert!(
        line.trim_start().starts_with("store i16 "),
        "Vec<u16>.push(0) must store two bytes, not a 4-byte i32: {}\nfull IR:\n{}",
        line,
        ir
    );
}

#[test]
fn vec_i32_push_of_a_typed_local_still_stores_i32() {
    // Sanity check on the fix itself: a same-width push must not be
    // truncated or otherwise altered by the new coercion path.
    let ir = compile_ir_source(
        r#"
        fn main() -> i32 {
          let mut buf: Vec<i32> = Vec::new()
          let x: i32 = 42
          buf.push(x)
          ret 0
        }
        "#,
    );
    let line = find_store_line(&ir, "%vec.push.dest");
    assert!(
        line.trim_start().starts_with("store i32 "),
        "Vec<i32>.push must still store i32 unchanged: {}\nfull IR:\n{}",
        line,
        ir
    );
}

// ---------------------------------------------------------------------------
// Runtime: the acceptance criteria.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod runtime {
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Command;

    use glyph_backend::codegen::CodegenContext;
    use glyph_backend::linker::{Linker, LinkerOptions};
    use glyph_frontend::{FrontendOptions, compile_source};
    use tempfile::TempDir;

    /// Compiles and links `source`, returning the executable path alongside
    /// the `TempDir` that must outlive it.
    fn build_exe(source: &str) -> (TempDir, PathBuf) {
        let frontend_output = compile_source(
            source,
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );
        assert!(
            frontend_output.diagnostics.is_empty(),
            "compilation failed: {:?}",
            frontend_output.diagnostics
        );

        let temp = TempDir::new().unwrap();
        let obj_path = temp.path().join("test.o");
        let exe_path = temp.path().join("test_exe");

        let mut ctx = CodegenContext::new("glyph_module").unwrap();
        ctx.codegen_module(&frontend_output.mir).unwrap();
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

        (temp, exe_path)
    }

    /// Runs the built executable once. Death by signal panics rather than
    /// being reported as a plain nonzero exit -- a crash is always a
    /// failure, and folding it into "nonzero" would let a SIGKILL from this
    /// exact bug read as an ordinary assertion failure.
    fn run_once(exe: &PathBuf) -> i32 {
        let output = Command::new(exe).output().unwrap();
        if let Some(signal) = output.status.signal() {
            panic!(
                "test binary was killed by signal {signal}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
            .status
            .code()
            .expect("exited without a code or a signal")
    }

    fn build_and_run_exit_code(source: &str) -> i32 {
        let (_temp, exe) = build_exe(source);
        run_once(&exe)
    }

    /// The ticket's exact repro, run 100 consecutive times against one
    /// build (the acceptance criterion). Before the fix this failed with
    /// SIGKILL roughly 10 times in 12; `run_once` turns any signal death
    /// into an immediate panic rather than a silent pass.
    #[test]
    fn repro_64_element_vec_u8_push_runs_clean_100_times() {
        let source = r#"
            from std/vec import Vec

            fn main() -> i32 {
              let mut buf: Vec<u8> = Vec::new()
              let mut i: i64 = 0
              while i < 64 {
                buf.push(0)
                i = i + 1
              }
              if buf.len() != 64 { ret 1 }
              ret 0
            }
        "#;
        let (_temp, exe) = build_exe(source);
        for run in 0..100 {
            let code = run_once(&exe);
            assert_eq!(code, 0, "run {run} exited {code}, expected 0");
        }
    }

    /// A million-element `Vec<u8>`, well past the 32-element threshold and
    /// past 20 growth doublings, with every element read back and checked
    /// (not just the boundaries) so a narrower corruption further inside
    /// the buffer would also be caught.
    #[test]
    fn million_element_vec_u8_holds_correct_values_throughout() {
        let source = r#"
            from std/vec import Vec

            fn main() -> i32 {
              let mut buf: Vec<u8> = Vec::new()
              let mut i: i64 = 0
              let n: i64 = 1000000
              while i < n {
                let b: u8 = (i % 256) as u8
                buf.push(b)
                i = i + 1
              }
              if buf.len() != 1000000 { ret 1 }

              let mut j: i64 = 0
              while j < n {
                let expected: u8 = (j % 256) as u8
                if buf[j as usize] != expected { ret 2 }
                j = j + 1
              }
              ret 0
            }
        "#;
        assert_eq!(build_and_run_exit_code(source), 0);
    }

    /// The same push-then-read-back shape as the repro, parameterised over
    /// every scalar width the ticket asked for: u8 and i8 (the narrow types
    /// that actually overran), u32 and i32 (exactly i32-width, so the
    /// coercion must be a no-op), u64 (wider than i32, including a value
    /// past the 32-bit boundary), and f64 (a non-integer path through the
    /// same store site).
    #[test]
    fn push_and_read_back_across_scalar_element_types() {
        let source = r#"
            from std/vec import Vec

            fn main() -> i32 {
              // i8: values straddling zero on both sides.
              let mut a: Vec<i8> = Vec::new()
              let mut i: i64 = 0
              while i < 64 {
                let v: i8 = ((i % 200) - 100) as i8
                a.push(v)
                i = i + 1
              }
              if a.len() != 64 { ret 1 }
              let a0: i8 = -100
              if a[0] != a0 { ret 2 }
              let a63: i8 = -37
              if a[63] != a63 { ret 3 }

              // u32: exactly i32-width, unsigned.
              let mut b: Vec<u32> = Vec::new()
              let mut j: i64 = 0
              while j < 64 {
                let v: u32 = (j * 1000) as u32
                b.push(v)
                j = j + 1
              }
              if b.len() != 64 { ret 4 }
              let b0: u32 = 0
              if b[0] != b0 { ret 5 }
              let b63: u32 = 63000
              if b[63] != b63 { ret 6 }

              // i32: exactly i32-width, signed -- the coercion must be a no-op.
              let mut c: Vec<i32> = Vec::new()
              let mut k: i64 = 0
              while k < 64 {
                c.push((k - 32) as i32)
                k = k + 1
              }
              if c.len() != 64 { ret 7 }
              if c[0] != -32 { ret 8 }
              if c[63] != 31 { ret 9 }

              // u64: wider than i32, including a value past the 32-bit boundary.
              let mut d: Vec<u64> = Vec::new()
              let mut m: i64 = 0
              while m < 64 {
                d.push((m * 100000000) as u64)
                m = m + 1
              }
              if d.len() != 64 { ret 10 }
              let d0: u64 = 0
              if d[0] != d0 { ret 11 }
              let d63: u64 = 6300000000
              if d[63] != d63 { ret 12 }

              // f64: a non-integer value through the same store site.
              let mut e: Vec<f64> = Vec::new()
              let mut p: i64 = 0
              while p < 64 {
                e.push((p as f64) * 1.5)
                p = p + 1
              }
              if e.len() != 64 { ret 13 }
              if e[0] != 0.0 { ret 14 }
              if e[63] != 94.5 { ret 15 }

              ret 0
            }
        "#;
        assert_eq!(build_and_run_exit_code(source), 0);
    }
}
