#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
use glyph_frontend::{FrontendOptions, compile_source};

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    )
}

fn compile_and_run(source: &str) -> i32 {
    let output = compile(source);
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mut codegen = CodegenContext::new("closure_tooling_acceptance").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.jit_execute_i32("main").unwrap()
}

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn closure_user_surface_runs_end_to_end() {
    let source = r#"
fn add_one(value: i32) -> i32 {
  ret value + 1
}

fn transform_one(callback: FnOnce<i32, i32>, value: i32) -> i32 {
  ret callback(value)
}

fn make_offset(offset: i32) -> FnOnce<i32, i32> {
  ret move value -> offset + value
}

fn main() -> i32 {
  let zero: FnOnce<(), i32> = () -> { 2 }
  let one: FnOnce<i32, i32> = value -> value + 3
  let many: FnOnce<(i32, i32), i32> = (left, right) -> {
    left + right
  }
  let returned = make_offset(10)
  let a = zero()
  let b = one(1)
  let c = many(5, 6)
  let d = transform_one(add_one, 10)
  let e = returned(4)
  ret a + b + c + d + e
}
"#;

    assert_eq!(compile_and_run(source), 42);
}

#[test]
fn ambiguous_closure_parameter_requests_context_or_annotation() {
    let diagnostics = messages(
        r#"
fn main() -> i32 {
  let identity = value -> value
  ret 0
}
"#,
    );

    assert!(
        diagnostics.iter().any(|message| {
            message.contains("cannot infer type of closure parameter 'value'")
                && message.contains("type annotation or a FnOnce context")
        }),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn owned_capture_cannot_be_used_after_closure_creation() {
    let diagnostics = messages(
        r#"
fn main() -> usize {
  let message: String = String::from_str("owned")
  let callback: FnOnce<(), usize> = move () -> message.len()
  ret message.len()
}
"#,
    );

    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("use of moved value `message`")),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn borrowed_capture_cannot_escape_by_return() {
    let diagnostics = messages(
        r#"
fn keep(view: str) -> FnOnce<(), str> {
  ret () -> view
}
"#,
    );

    assert!(
        diagnostics.iter().any(|message| {
            message.contains("borrowed capture `view` cannot escape through return")
        }),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn recursive_closure_cycle_is_rejected() {
    let diagnostics = messages(
        r#"
fn main() -> i32 {
  let callback: FnOnce<(), i32> = () -> callback()
  ret callback()
}
"#,
    );

    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("recursive closure cycle through `callback`")),
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn borrowed_callable_kinds_are_explicitly_deferred() {
    for callable in ["Fn<i32, i32>", "FnMut<i32, i32>"] {
        let diagnostics = messages(&format!(
            r#"
fn main() -> i32 {{
  let callback: {callable} = value -> value
  ret 0
}}
"#
        ));

        assert!(
            diagnostics
                .iter()
                .any(|message| { message.contains("not supported") && message.contains("FnOnce") }),
            "{callable} diagnostics: {diagnostics:?}"
        );
    }
}
