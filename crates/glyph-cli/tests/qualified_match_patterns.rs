//! GLYPH-13: qualified match patterns (`Enum::Variant`) used to parse but
//! silently discard the qualifier, keeping only the final segment
//! (`crates/glyph-frontend/src/parser/expr.rs`'s old `parse_match_pattern`
//! looped over every `::` segment and threw away everything but the last).
//! That meant a pattern like `B::X` would happily match against a
//! same-named variant on a completely different enum `A`, as long as the
//! scrutinee's actual type had a variant literally named `X`.
//!
//! The release semantics implemented here:
//! 1. A qualified pattern `E::V` must resolve against enum `E`. If the
//!    scrutinee's actual enum type is different from `E` (or `E` doesn't
//!    exist at all), that's a diagnosed error, not a silent match against
//!    the wrong enum.
//! 2. An unqualified pattern `V` resolves against the scrutinee's enum type,
//!    which is always statically known by the time `match` lowers its arms
//!    (a match scrutinee must already be a fully-typed enum value) - this is
//!    unchanged from before.
//! 3. Existing unqualified (`Some(x)`, `None`, `Ok(v)`, `Err(e)`) and
//!    qualified (`Val::Nil`-style) patterns keep working unchanged.

use std::fs;
use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

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

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ---------------------------------------------------------------------
// Same-module: two enums declaring a same-named variant.
// ---------------------------------------------------------------------

/// Two enums in one module share a variant name (`X`). Both the qualified
/// (`A::X`, `B::X`) and unqualified (`X`) spellings must resolve against
/// each match's own scrutinee type, without cross-contamination.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn same_named_variant_across_two_enums_qualified_and_unqualified() {
    let source = r#"
        enum A {
          X,
          Y,
        }

        enum B {
          X(i32),
          Z,
        }

        fn use_a_qualified(v: A) -> i32 {
          ret match v {
            A::X => 10,
            A::Y => 20,
          }
        }

        fn use_a_unqualified(v: A) -> i32 {
          ret match v {
            X => 10,
            Y => 20,
          }
        }

        fn use_b_qualified(v: B) -> i32 {
          ret match v {
            B::X(n) => n,
            B::Z => 0,
          }
        }

        fn use_b_unqualified(v: B) -> i32 {
          ret match v {
            X(n) => n,
            Z => 0,
          }
        }

        fn main() -> i32 {
          if use_a_qualified(A::X) != 10 { ret 1 }
          if use_a_qualified(A::Y) != 20 { ret 2 }
          if use_a_unqualified(A::X) != 10 { ret 3 }
          if use_a_unqualified(A::Y) != 20 { ret 4 }
          if use_b_qualified(B::X(7)) != 7 { ret 5 }
          if use_b_qualified(B::Z) != 0 { ret 6 }
          if use_b_unqualified(B::X(9)) != 9 { ret 7 }
          if use_b_unqualified(B::Z) != 0 { ret 8 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------
// Cross-module: two modules each declare an enum with the same variant name.
// ---------------------------------------------------------------------

/// Same as above, but `A` and `B` live in separate modules and are pulled in
/// via `from <module> import <Enum>`. Qualified patterns must still resolve
/// against the imported enum's own (local) name.
#[test]
fn same_named_variant_across_two_modules_qualified_pattern() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    write_file(
        &root.join("mod_a.glyph"),
        r#"enum A {
  X,
  Y,
}
"#,
    );
    write_file(
        &root.join("mod_b.glyph"),
        r#"enum B {
  X(i32),
  Z,
}
"#,
    );
    write_file(
        &root.join("main.glyph"),
        r#"from mod_a import A
from mod_b import B
from std import println

fn use_a(v: A) -> i32 {
  ret match v {
    A::X => 10,
    A::Y => 20,
  }
}

fn use_b(v: B) -> i32 {
  ret match v {
    B::X(n) => n,
    B::Z => 0,
  }
}

fn main() -> i32 {
  if use_a(A::X) != 10 {
    println("fail: A::X")
    ret 1
  }
  if use_b(B::X(9)) != 9 {
    println("fail: B::X")
    ret 2
  }
  if use_b(B::Z) != 0 {
    println("fail: B::Z")
    ret 3
  }
  ret 0
}
"#,
    );

    // `path.parent()` on a bare filename (no directory component) yields an
    // empty path, which the loader then fails to read from; always pass a
    // path with an explicit directory component.
    let mut cmd = cargo_bin_cmd!("glyph-cli");
    cmd.current_dir(root)
        .arg("build")
        .arg("./main.glyph")
        .arg("--emit")
        .arg("exe")
        .assert()
        .success();

    let exe = root.join("main");
    let output = Command::new(&exe).output().unwrap();
    assert!(
        output.status.success(),
        "exe exited with {:?}, stdout: {}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------
// Error cases: qualifier mismatch and unknown qualifier.
// ---------------------------------------------------------------------

/// A pattern qualified with a DIFFERENT (but real) enum than the scrutinee's
/// actual type must be a diagnosed error naming both enums, not a silent
/// match against the scrutinee's enum.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn qualifier_naming_a_different_enum_is_diagnosed() {
    let diags = diagnostics_for(
        r#"
        enum A {
          X,
          Y,
        }

        enum B {
          X(i32),
          Z,
        }

        fn use_a(v: A) -> i32 {
          ret match v {
            B::X => 10,
            A::Y => 20,
          }
        }

        fn main() -> i32 {
          ret use_a(A::X)
        }
    "#,
    );

    assert!(
        diags
            .iter()
            .any(|d| d.contains("pattern qualifier 'B'") && d.contains("'A'")),
        "diags: {:?}",
        diags
    );
}

/// A pattern qualified with an enum name that doesn't exist at all must be
/// diagnosed as an unknown enum, not silently fall back to the scrutinee's
/// actual type.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn qualifier_naming_an_unknown_enum_is_diagnosed() {
    let diags = diagnostics_for(
        r#"
        enum A {
          X,
          Y,
        }

        fn use_a(v: A) -> i32 {
          ret match v {
            Zzz::X => 10,
            A::Y => 20,
          }
        }

        fn main() -> i32 {
          ret use_a(A::X)
        }
    "#,
    );

    assert!(
        diags.iter().any(|d| d.contains("unknown enum type 'Zzz'")),
        "diags: {:?}",
        diags
    );
}

// ---------------------------------------------------------------------
// Regressions: existing unqualified stdlib patterns and qualified
// `Val::Nil`-style custom-enum patterns must keep working unchanged.
// ---------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn unqualified_stdlib_option_and_result_patterns_still_work() {
    let source = r#"
        from std/enums import Option
        from std/enums import Result

        fn opt(o: Option<i32>) -> i32 {
          ret match o {
            Some(x) => x,
            None => -1,
          }
        }

        fn res(r: Result<i32, i32>) -> i32 {
          ret match r {
            Ok(x) => x,
            Err(e) => 0 - e,
          }
        }

        fn main() -> i32 {
          if opt(Some(3)) != 3 { ret 1 }
          if opt(None) != -1 { ret 2 }
          if res(Ok(5)) != 5 { ret 3 }
          if res(Err(2)) != -2 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn qualified_custom_enum_pattern_still_works() {
    let source = r#"
        enum Val {
          Nil,
          Text(String),
        }

        fn describe(v: Val) -> i32 {
          ret match v {
            Val::Nil => 0,
            Val::Text(_s) => 1,
          }
        }

        fn main() -> i32 {
          if describe(Val::Nil) != 0 { ret 1 }
          if describe(Val::Text(String::from_str("x"))) != 1 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
