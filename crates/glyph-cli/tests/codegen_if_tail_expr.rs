//! GLYPH-84: a bare `if`/`else` occupying a function's own tail position
//! (no `ret`, no enclosing `let`) must produce the branch's value as the
//! function's return value, exactly the way `match` already does in that
//! same position.
//!
//! Before the fix, `lower_function` lowered the body block with
//! `control_value_context = false`, so a tail `Expr::If` took the
//! statement-only path (`lower_if`) instead of the value path
//! (`lower_if_value`): both branches computed their literal and threw it
//! away, and the function fell through to an implicit `Return(None)`,
//! which codegen turns into a zeroed/default value. Every call site got
//! the same wrong default silently - no diagnostic, no crash.
//!
//! Coverage here:
//! - the exact ticket repro, plus i64/f64/bool/String/struct/enum returns
//! - nested `if`/`else if`/`else` chains
//! - branches that are blocks with statements before the tail value
//! - a tail `if` inside a closure body
//! - a tail `if` *without* `else` in a non-void function is now a
//!   diagnostic instead of a silent wrong value
//! - a tail `if`/`else` (with and without `else`) in a *void* function
//!   still works (the GLYPH-65 regression surface)

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

fn diagnostics_for(source: &str) -> Vec<String> {
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    output
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

/// Compile, link and run `source` as a native binary, reporting the process
/// exit code. A death by signal is reported as a negative code rather than
/// silently treated as success.
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
        panic!("test binary was killed by signal {sig}");
    } else {
        -1
    }
}

// ---------------------------------------------------------------------------
// The ticket's own repro, and one return type per scalar/aggregate family.
// Each program returns 0 on success and a distinct nonzero code identifying
// which check failed, so a regression points straight at the failing case.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_i32_runtime() {
    // The exact GLYPH-84 repro: `pick(0)` must be 100, `pick(1)` must be 300.
    let source = r#"
        fn pick(n: i32) -> i32 {
          if n == 0 { 100 } else { 300 }
        }

        fn main() -> i32 {
          if pick(0) != 100 { ret 1 }
          if pick(1) != 300 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_i64_runtime() {
    let source = r#"
        fn pick(n: i32) -> i64 {
          if n == 0 { 100 } else { 300 }
        }

        fn main() -> i32 {
          if pick(0) != 100 { ret 1 }
          if pick(1) != 300 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_f64_runtime() {
    let source = r#"
        fn pick(n: i32) -> f64 {
          if n == 0 { 1.5 } else { 3.5 }
        }

        fn main() -> i32 {
          if pick(0) != 1.5 { ret 1 }
          if pick(1) != 3.5 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_bool_runtime() {
    let source = r#"
        fn pick(n: i32) -> bool {
          if n == 0 { true } else { false }
        }

        fn main() -> i32 {
          if pick(0) != true { ret 1 }
          if pick(1) != false { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_string_runtime() {
    // `$"..."` with no interpolation holes still allocates a real owned
    // String (unlike a bare string literal coerced to `String`, which hits
    // an unrelated, pre-existing double-free bug independent of GLYPH-84 -
    // reproducible even through the documented `ret if ... else ...`
    // workaround). The result is bound to a `let` before comparing: comparing
    // an unbound `String`-returning call result inline (`pick(0) != "zero"`)
    // hits a second, likewise pre-existing and unrelated temporary-lifetime
    // bug, independent of `if`/`else` entirely (reproduces with a plain
    // non-branching function too), so neither is exercised here.
    let source = r#"
        fn pick(n: i32) -> String {
          if n == 0 { $"zero" } else { $"other" }
        }

        fn main() -> i32 {
          let s0 = pick(0)
          if s0 != "zero" { ret 1 }
          let s1 = pick(1)
          if s1 != "other" { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_struct_runtime() {
    let source = r#"
        struct Point {
          x: i32
          y: i32
        }

        fn pick(n: i32) -> Point {
          if n == 0 { Point { x: 1, y: 2 } } else { Point { x: 9, y: 8 } }
        }

        fn main() -> i32 {
          let p0 = pick(0)
          if p0.x != 1 { ret 1 }
          if p0.y != 2 { ret 2 }
          let p1 = pick(1)
          if p1.x != 9 { ret 3 }
          if p1.y != 8 { ret 4 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_tail_if_else_option_enum_runtime() {
    let source = r#"
        from std/enums import Option

        fn pick(n: i32) -> Option<i32> {
          if n == 0 { Some(42) } else { None() }
        }

        fn main() -> i32 {
          let a = match pick(0) {
            Some(v) => v,
            None => -1,
          }
          if a != 42 { ret 1 }
          let b = match pick(1) {
            Some(_v) => -1,
            None => 0,
          }
          if b != 0 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Structural coverage: nested chains, blocks with statements, closures.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_if_else_if_chain_tail_runtime() {
    let source = r#"
        fn classify(n: i32) -> i32 {
          if n == 0 {
            10
          } else if n == 1 {
            20
          } else if n == 2 {
            30
          } else {
            40
          }
        }

        fn main() -> i32 {
          if classify(0) != 10 { ret 1 }
          if classify(1) != 20 { ret 2 }
          if classify(2) != 30 { ret 3 }
          if classify(3) != 40 { ret 4 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_if_branches_with_statements_before_the_value_runtime() {
    let source = r#"
        fn compute(n: i32) -> i32 {
          if n == 0 {
            let a = 10
            let b = 20
            a + b
          } else {
            let c = 1
            let d = c + 1
            d * 100
          }
        }

        fn main() -> i32 {
          if compute(0) != 30 { ret 1 }
          if compute(1) != 200 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_if_inside_closure_body_runtime() {
    let source = r#"
        fn main() -> i32 {
          let f: Fn<i32, i32> = (n: i32) -> {
            if n == 0 { 7 } else { 9 }
          }
          if f(0) != 7 { ret 1 }
          if f(1) != 9 { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// A tail `if` with no `else` in a non-void function has no value to give
// back; it must be a diagnostic, never a silent default.
// ---------------------------------------------------------------------------

#[test]
fn tail_if_without_else_in_non_void_function_is_diagnosed() {
    let diags = diagnostics_for(
        r#"
        fn pick(n: i32) -> i32 {
          if n == 0 { 100 }
        }

        fn main() -> i32 {
          ret pick(0)
        }
        "#,
    );
    assert!(
        !diags.is_empty(),
        "expected a diagnostic for a tail `if` with no `else` in a non-void function"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("missing an else branch") || d.contains("else")),
        "diagnostics did not mention the missing else branch: {:?}",
        diags
    );
}

#[test]
fn tail_if_without_else_diagnostic_also_fires_through_a_typed_let() {
    // The same gap existed at the `lower_if_value` level generally (not just
    // for a function's own tail position): `let v: i32 = if cond { 1 };`
    // used to silently bind a unit value where an i32 was required.
    let diags = diagnostics_for(
        r#"
        fn pick(n: i32) -> i32 {
          let v: i32 = if n == 0 { 100 }
          ret v
        }

        fn main() -> i32 {
          ret pick(0)
        }
        "#,
    );
    assert!(
        !diags.is_empty(),
        "expected a diagnostic for a typed `let` bound to an else-less `if`"
    );
}

// ---------------------------------------------------------------------------
// GLYPH-65 regression surface: a tail `if`, with or without `else`, in a
// *void* function must keep working (this is what routing the function
// body through the value-producing lowering path could plausibly break).
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_if_else_in_void_function_runtime() {
    let source = r#"
        from std import println

        fn announce(n: i32) {
          if n == 0 {
            println("zero")
          } else {
            println("other")
          }
        }

        fn main() -> i32 {
          announce(0)
          announce(1)
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_if_without_else_in_void_function_runtime() {
    let source = r#"
        from std import println

        fn maybe_announce(n: i32) {
          if n == 0 {
            println("zero-only")
          }
        }

        fn main() -> i32 {
          maybe_announce(0)
          maybe_announce(1)
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}
