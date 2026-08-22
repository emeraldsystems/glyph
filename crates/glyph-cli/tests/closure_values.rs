#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
#[cfg(unix)]
use glyph_backend::linker::{Linker, LinkerOptions};
use glyph_frontend::{FrontendOptions, compile_source};

fn compile_and_run(source: &str) -> i32 {
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mut codegen = CodegenContext::new("closure_source_e2e").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.jit_execute_i32("main").unwrap()
}

#[test]
fn captured_source_closure_executes_through_lifted_mir() {
    assert_eq!(
        compile_and_run(
            r#"
fn main() -> i32 {
  let base: i32 = 40
  let add: FnOnce<i32, i32> = (value: i32) -> base + value
  ret add(2)
}
"#,
        ),
        42
    );
}

#[test]
fn nested_source_closure_executes_with_transitive_captures() {
    assert_eq!(
        compile_and_run(
            r#"
fn main() -> i32 {
  let base: i32 = 40
  let outer: FnOnce<i32, FnOnce<i32, i32>> =
    (left: i32) -> (right: i32) -> base + left + right
  let inner: FnOnce<i32, i32> = outer(1)
  ret inner(1)
}
"#,
        ),
        42
    );
}

#[cfg(unix)]
#[test]
fn capturing_closure_links_and_executes_as_a_native_object() {
    let source = r#"
fn main() -> i32 {
  let base: i32 = 40
  let add: FnOnce<i32, i32> = (value: i32) -> base + value
  ret add(2)
}
"#;
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("closure.o");
    let executable = temp.path().join("closure");
    let mut codegen = CodegenContext::new("closure_source_object").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object],
            link_libs: vec![],
            link_search_paths: vec![],
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();

    assert_eq!(
        std::process::Command::new(executable)
            .status()
            .unwrap()
            .code(),
        Some(42)
    );
}
