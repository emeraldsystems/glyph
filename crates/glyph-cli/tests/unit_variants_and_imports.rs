//! Regressions for two single-module resolution bugs:
//!
//! 1. Bare unit-variant paths (`Val::Nil`, `None`) silently failed to lower
//!    in single-module builds — only the call form (`Val::Text(x)`) worked.
//!    They now resolve through the enum-constructor signatures.
//! 2. `import std` followed by `from std/x import Y` was misparsed: the
//!    lookahead for the `import a, b from m` form crossed the line break,
//!    swallowed the next statement's `from`, and turned the trailing
//!    `import Y` into a bogus wildcard import ("module 'Y' not found").
//!    Import lookahead is now line-bounded.

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

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_unit_variant_constructs() {
    let source = r#"
        enum Val {
          Nil,
          Text(String),
        }

        fn main() -> i32 {
          let n = Val::Nil
          ret match n {
            Nil => 0,
            Text(_s) => 1,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_of_user_enum_push_pop() {
    let source = r#"
        from std/vec import Vec
        from std/enums import Option

        enum Val {
          Nil,
          Text(String),
        }

        fn main() -> i32 {
          let mut v: Vec<Val> = Vec::new()
          v.push(Val::Nil)
          v.push(Val::Text(String::from_str("x")))
          if v.len() != 2 { ret 1 }
          let a = v.pop()
          ret match a {
            Some(av) => match av {
              Text(_s) => 0,
              Nil => 2,
            },
            None => 3,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn bare_none_with_annotation() {
    let source = r#"
        from std/enums import Option

        fn main() -> i32 {
          let o: Option<i32> = None
          ret match o {
            Some(_x) => 1,
            None => 0,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn wildcard_import_before_selective_imports() {
    let source = r#"import std
from std/vec import Vec
from std/enums import Option

enum Val {
  Nil,
  Text(String),
}

fn main() -> i32 {
  let mut v: Vec<Val> = Vec::new()
  v.push(Val::Nil)
  if v.len() != 1 { ret 1 }
  let o: Option<i32> = Some(9)
  ret match o {
    Some(x) => x - 9,
    None => 2,
  }
}
"#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// The JS-style single-line form must keep working after the lookahead
// became line-bounded.
#[test]
fn single_line_import_from_form_still_parses() {
    let source = r#"import println from std

fn main() -> i32 {
  println("hi")
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
}
