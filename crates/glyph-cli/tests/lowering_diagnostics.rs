//! MIR lowering is no longer allowed to fail silently: any statement whose
//! expression cannot be lowered produces a diagnostic instead of quietly
//! becoming a Nop (the pattern that let float literals, char literals, and
//! unit enum variants miscompile silently). Also covers out-of-range integer
//! literals, which previously parsed as 0.

use glyph_frontend::{FrontendOptions, compile_source};

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

#[test]
fn unlowered_let_initializer_is_reported_once() {
    let diags = diagnostics_for(
        r#"import std

fn main() -> i32 {
  let x = does_not_exist
  ret 0
}
"#,
    );
    assert_eq!(diags.len(), 1, "diags: {:?}", diags);
    assert!(
        diags[0].contains("let initializer could not be lowered"),
        "unexpected message: {}",
        diags[0]
    );
}

#[test]
fn unlowered_expression_statement_is_reported() {
    let diags = diagnostics_for(
        r#"import std

fn main() -> i32 {
  mystery_name
  ret 0
}
"#,
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("expression statement could not be lowered")),
        "diags: {:?}",
        diags
    );
}

#[test]
fn already_diagnosed_errors_are_not_double_reported() {
    let diags = diagnostics_for(
        r#"import std

fn main() -> i32 {
  let x: i32 = unknown_fn(3);
  ret 0
}
"#,
    );
    assert_eq!(diags.len(), 1, "diags: {:?}", diags);
    assert!(
        diags[0].contains("unknown function 'unknown_fn'"),
        "unexpected message: {}",
        diags[0]
    );
}

#[test]
fn unlowered_assignment_value_is_reported() {
    let diags = diagnostics_for(
        r#"import std

fn main() -> i32 {
  let mut x: i32 = 1;
  x = ghost_value
  ret x
}
"#,
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("assignment value could not be lowered")),
        "diags: {:?}",
        diags
    );
}

#[test]
fn integer_literal_beyond_u64_is_rejected() {
    let diags = diagnostics_for(
        r#"fn main() -> i32 {
  let x: u64 = 99999999999999999999999999;
  ret 0
}
"#,
    );
    assert!(
        diags.iter().any(|d| d.contains("out of range (max u64)")),
        "diags: {:?}",
        diags
    );
}

// Regression: u64 literals above i64::MAX used to silently become 0.
#[cfg(all(feature = "codegen", unix))]
mod exec {
    use glyph_backend::{
        codegen::CodegenContext,
        linker::{Linker, LinkerOptions},
    };
    use glyph_frontend::{FrontendOptions, compile_source};
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn u64_max_literal_round_trips() {
        if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
            return;
        }

        let source = r#"
            fn main() -> i32 {
              let big: u64 = 18446744073709551615;
              let low: u32 = big as u32;
              if low != 4294967295 { ret 1 }
              ret 0
            }
        "#;

        let frontend_output = compile_source(
            source,
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );
        assert!(
            frontend_output.diagnostics.is_empty(),
            "diagnostics: {:?}",
            frontend_output.diagnostics
        );

        let temp = TempDir::new().unwrap();
        let obj_path = temp.path().join("test.o");
        let exe_path = temp.path().join("test_exe");

        let mut ctx = CodegenContext::new("glyph_module").unwrap();
        ctx.codegen_module(&frontend_output.mir).unwrap();
        if std::env::var("GLYPH_SKIP_RUN").is_ok() {
            return;
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
        assert_eq!(status.code(), Some(0));
    }
}
