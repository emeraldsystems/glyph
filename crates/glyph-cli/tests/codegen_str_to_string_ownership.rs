//! GLYPH-87: a borrowed `str` view that lands in an owned `String` slot must
//! be heap-copied exactly once.
//!
//! `str` and `String` share one LLVM representation (a `char*`), so codegen
//! cannot tell a pointer into the binary's read-only data from a `malloc`ed
//! one. Before the fix, `fn f() -> String { "x" }` (and `ret "x"`, a `str`
//! parameter returned as `String`, a literal passed to a `String` parameter,
//! a literal pushed into a `Vec<String>`, ...) handed the caller the literal
//! itself, and whoever eventually dropped the `String` called `free` on it:
//!
//! ```text
//! malloc: *** error for object 0x...: pointer being freed was not allocated
//! ```
//!
//! The crash was masked in the most common shape - `let s = f(); if s != "x"`
//! - by a second bug: a string comparison *moved* its `String` operand, so
//! the bogus pointer was never freed (and `s` could not be used again).
//! Both are fixed together: every expected-typed lowering path now funnels
//! through one choke point that emits a `StringClone` for a `str` view
//! headed into a `String` slot, and comparisons only read their operands.
//!
//! Every runtime test here runs the program under macOS guard-malloc
//! (`MallocScribble` / `MallocGuardEdges`), which turns a free of read-only
//! data or a double free into an abort rather than a silent no-op.

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

/// Compile `source` to LLVM IR text (no linking, no run).
#[cfg(all(feature = "codegen", unix))]
fn ir_for(source: &str) -> String {
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
    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();
    ctx.dump_ir()
}

/// The body of `define ... @<name>(` in `ir`, up to its closing brace.
#[cfg(all(feature = "codegen", unix))]
fn function_body<'a>(ir: &'a str, name: &str) -> &'a str {
    let needle = format!("@{name}(");
    let start = ir
        .match_indices("define ")
        .map(|(i, _)| i)
        .find(|&i| ir[i..].lines().next().unwrap_or("").contains(&needle))
        .unwrap_or_else(|| panic!("no definition of {name} in IR:\n{ir}"));
    let end = ir[start..]
        .find("\n}")
        .map(|e| start + e)
        .unwrap_or(ir.len());
    &ir[start..end]
}

/// Compile, link and run `source` as a native binary under guard-malloc,
/// reporting the process exit code. A death by signal (guard-malloc aborts
/// with SIGABRT) is reported as a panic rather than silently treated as
/// success.
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

    let status = Command::new(&exe_path)
        // Harmless on non-macOS libcs; on macOS they make a free of
        // read-only data or a double free abort deterministically.
        .env("MallocScribble", "1")
        .env("MallocGuardEdges", "1")
        .status()
        .unwrap();
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        panic!("test binary was killed by signal {sig} (guard-malloc abort?)");
    } else {
        -1
    }
}

// ---------------------------------------------------------------------------
// The two ticket repros.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_literal_string_return_runtime() {
    // Ticket shape 1: a `String` function whose tail is a bare literal. The
    // result is compared twice and then dropped at scope exit: the first
    // compare must not move it, and the drop must free a heap copy.
    let source = r#"
        fn f() -> String { "x" }

        fn main() -> i32 {
          let s = f()
          if s != "x" { ret 1 }
          if s != "x" { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn inline_compare_of_string_call_result_runtime() {
    // Ticket shape 2: the call temporary is never bound, so it is dropped at
    // the end of each `ret` path - against a literal, another call, and a
    // `str` local.
    let source = r#"
        fn f() -> String { "x" }
        fn g() -> String { "x" }
        fn h() -> String { "y" }

        fn main() -> i32 {
          let view: str = "x"
          if f() != "x" { ret 1 }
          if f() != g() { ret 2 }
          if f() == h() { ret 3 }
          if f() != view { ret 4 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Every other place a `str` view is first treated as an owned `String`.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn explicit_ret_literal_string_runtime() {
    let source = r#"
        fn f() -> String { ret "explicit" }

        fn main() -> i32 {
          if f() != "explicit" { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn str_param_returned_as_string_runtime() {
    // The view is a parameter, not a literal: still a `str` that must be
    // copied before the caller owns it.
    let source = r#"
        fn own(s: str) -> String { s }
        fn own_ret(s: str) -> String { ret s }

        fn main() -> i32 {
          if own("p") != "p" { ret 1 }
          if own_ret("q") != "q" { ret 2 }
          let lit: str = "view"
          let owned: String = lit
          if owned != lit { ret 3 }
          if owned != "view" { ret 4 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn if_else_and_match_arm_literal_string_runtime() {
    // Branch values reach the join local through the same expected-typed
    // path, so each arm copies its own literal.
    let source = r#"
        enum Kind { A, B }

        fn pick(n: i32) -> String {
          if n == 0 { "zero" } else { "other" }
        }

        fn name(k: Kind) -> String {
          match k {
            Kind::A => "a",
            Kind::B => "b",
          }
        }

        fn main() -> i32 {
          if pick(0) != "zero" { ret 1 }
          if pick(1) != "other" { ret 2 }
          if name(Kind::A) != "a" { ret 3 }
          if name(Kind::B) != "b" { ret 4 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn literal_passed_to_string_param_runtime() {
    // The callee owns and drops its parameter; a literal argument must be
    // copied at the call site.
    let source = r#"
        fn takes(s: String) -> i32 {
          if s != "arg" { ret 1 }
          if s != "arg" { ret 2 }
          ret 0
        }

        fn main() -> i32 {
          if takes("arg") != 0 { ret 1 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn closure_returning_literal_string_runtime() {
    let source = r#"
        fn main() -> i32 {
          let f: FnOnce<(), String> = () -> "closed"
          if f() != "closed" { ret 1 }
          let g: Fn<i32, String> = (n: i32) -> { if n == 1 { "one" } else { "many" } }
          if g(1) != "one" { ret 2 }
          if g(2) != "many" { ret 3 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn string_local_assigned_literal_runtime() {
    // `let mut s: String = "a"; s = "b"`: both the initializer and the
    // reassignment are `String` slots; the old value is dropped on
    // reassignment and the new one at scope exit.
    let source = r#"
        fn main() -> i32 {
          let mut s: String = "first"
          if s != "first" { ret 1 }
          s = "second"
          if s != "second" { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_string_push_literal_runtime() {
    // `Vec<String>` frees each element on drop: a pushed literal must be a
    // heap copy.
    let source = r#"
        fn main() -> i32 {
          let mut v: Vec<String> = Vec::new()
          v.push("a")
          v.push("b")
          let s0: str = v[0]
          let s1: str = v[1]
          if s0 != "a" { ret 1 }
          if s1 != "b" { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn enum_payload_literal_string_runtime() {
    // `Some("x")` into an `Option<String>` (and `Ok`/`Err` into a
    // `Result<String, String>`): the constructor's payload slot is typed by
    // the enum's type parameter, which is substituted from the expected
    // result type, so the literal is copied into the owned payload.
    let source = r#"
        from std/enums import Option, Result

        fn maybe(n: i32) -> Option<String> {
          if n == 0 { Some("some") } else { None }
        }

        fn res(n: i32) -> Result<String, String> {
          if n == 0 { Ok("ok") } else { Err("err") }
        }

        fn main() -> i32 {
          let o: Option<String> = Some("x")
          let a = maybe(0)
          match a {
            Some(s) => { if s != "some" { ret 1 } },
            None => { ret 2 },
          }
          let b = res(0)
          match b {
            Ok(s) => { if s != "ok" { ret 3 } },
            Err(_e) => { ret 4 },
          }
          let c = res(1)
          match c {
            Ok(_s) => { ret 5 },
            Err(e) => { if e != "err" { ret 6 } },
          }
          match o {
            Some(s) => { if s != "x" { ret 7 } },
            None => { ret 8 },
          }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn let_bound_forms_unchanged_runtime() {
    // The forms the ticket recommended as workarounds keep working, and an
    // already-owned `String` returned from a `let` is moved out, not copied
    // again (see the IR test below).
    let source = r#"
        fn f() -> String { let s: String = "x"; s }

        fn main() -> i32 {
          let s = f()
          if s != "x" { ret 1 }
          if f() != "x" { ret 2 }
          ret 0
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// Comparisons read, they do not move.
// ---------------------------------------------------------------------------

#[test]
fn comparison_does_not_move_string_operand() {
    // Before the fix this was rejected with "use of moved value `s`".
    let source = r#"
        fn f() -> String { "x" }
        fn takes(s: String) -> i32 { ret 0 }

        fn main() -> i32 {
          let s = f()
          if s != "x" { ret 1 }
          if s == "x" { ret takes(s) }
          ret 2
        }
    "#;
    assert_eq!(diagnostics_for(source), Vec::<String>::new());
}

#[test]
fn comparison_of_moved_string_is_still_rejected() {
    // Reading a moved `String` in a comparison is still a use-after-move.
    let source = r#"
        fn f() -> String { "x" }
        fn takes(s: String) -> i32 { ret 0 }

        fn main() -> i32 {
          let s = f()
          let _ = takes(s)
          if s != "x" { ret 1 }
          ret 0
        }
    "#;
    let diags = diagnostics_for(source);
    assert!(
        diags.iter().any(|d| d.contains("use of moved value `s`")),
        "expected a use-after-move diagnostic, got {diags:?}"
    );
}

// ---------------------------------------------------------------------------
// IR: the copy happens exactly once.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn tail_literal_string_copied_exactly_once_ir() {
    let ir = ir_for("fn f() -> String { \"x\" }\n\nfn main() -> i32 { ret 0 }\n");
    let f = function_body(&ir, "f");
    assert_eq!(
        f.matches("@malloc(").count(),
        1,
        "expected exactly one heap allocation for the returned literal:\n{f}"
    );
    assert_eq!(
        f.matches("@memcpy(").count(),
        1,
        "expected exactly one copy of the literal bytes:\n{f}"
    );
    assert!(
        !f.contains("ret ptr @.str"),
        "the literal itself must not be returned:\n{f}"
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn owned_string_returned_from_let_not_recopied_ir() {
    // The `let` initializer makes the one copy; returning the owned local
    // moves it and must not add a second one.
    let ir = ir_for("fn f() -> String { let s: String = \"x\"; s }\n\nfn main() -> i32 { ret 0 }\n");
    let f = function_body(&ir, "f");
    assert_eq!(
        f.matches("@malloc(").count(),
        1,
        "expected exactly one heap allocation:\n{f}"
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn str_returned_as_str_is_not_copied_ir() {
    // No coercion when the slot is `str` too.
    let ir = ir_for("fn f() -> str { \"x\" }\n\nfn main() -> i32 { ret 0 }\n");
    let f = function_body(&ir, "f");
    assert_eq!(
        f.matches("@malloc(").count(),
        0,
        "a str-to-str return must not allocate:\n{f}"
    );
}
