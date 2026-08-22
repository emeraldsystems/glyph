use glyph_backend::codegen::CodegenContext;
use glyph_core::mir::Rvalue;
use glyph_frontend::{FrontendOptions, compile_source};
use std::collections::HashMap;

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
    let mut codegen = CodegenContext::new("callable_source_test").unwrap();
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
fn named_function_value_can_be_passed_to_callable_parameter() {
    let source = r#"
fn identity(value: i32) -> i32 {
  ret value
}

fn apply(function: FnOnce<i32, i32>, value: i32) -> i32 {
  ret function(value)
}

fn main() -> i32 {
  ret apply(identity, 42)
}
"#;

    assert_eq!(compile_and_run(source), 42);
    let output = compile(source);
    let apply = output
        .mir
        .functions
        .iter()
        .find(|function| function.name == "apply")
        .unwrap();
    assert!(
        apply
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| {
                matches!(
                    inst,
                    glyph_core::mir::MirInst::Assign {
                        value: Rvalue::CallIndirect { .. },
                        ..
                    }
                )
            })
    );
    let main = output
        .mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap();
    assert!(
        main.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| {
                matches!(
                    inst,
                    glyph_core::mir::MirInst::Assign {
                        value: Rvalue::Call { name, .. },
                        ..
                    } if name == "apply"
                )
            })
    );
}

#[test]
fn callable_can_be_returned_and_called_with_multiple_arguments() {
    let source = r#"
fn add(left: i32, right: i32) -> i32 {
  ret left + right
}

fn make_adder() -> FnOnce<(i32, i32), i32> {
  ret add
}

fn main() -> i32 {
  let function = make_adder()
  ret function(19, 23)
}
"#;

    assert_eq!(compile_and_run(source), 42);
}

#[test]
fn callable_supports_one_tuple_parameter() {
    let source = r#"
fn sum_pair(pair: (i32, i32)) -> i32 {
  ret pair.0 + pair.1
}

fn main() -> i32 {
  let function: FnOnce<((i32, i32),), i32> = sum_pair
  ret function((19, 23))
}
"#;

    assert_eq!(compile_and_run(source), 42);
}

#[test]
fn callable_supports_unit_and_tuple_results() {
    let unit_source = r#"
fn ping() {}
fn main() -> i32 {
  let function = ping
  function()
  ret 42
}
"#;
    assert_eq!(compile_and_run(unit_source), 42);

    let tuple_source = r#"
fn make_pair() -> (i32, i32) { ret (19, 23) }
fn main() -> i32 {
  let function = make_pair
  let pair = function()
  ret pair.0 + pair.1
}
"#;
    assert_eq!(compile_and_run(tuple_source), 42);
}

#[test]
fn callable_supports_large_aggregate_argument_and_sret_result() {
    let source = r#"
struct Big {
  a: i32,
  b: i32,
  c: i32,
  d: i32,
  e: i32
}

fn identity_big(value: Big) -> Big {
  ret value
}

fn main() -> i32 {
  let function = identity_big
  let value = Big { a: 1, b: 2, c: 3, d: 4, e: 42 }
  let result = function(value)
  ret result.e
}
"#;

    assert_eq!(compile_and_run(source), 42);
}

#[test]
fn callable_supports_owned_string_result() {
    let source = r#"
fn make_message() -> String {
  ret String::from_str("forty-two")
}

fn main() -> i32 {
  let function = make_message
  let message = function()
  ret message.len()
}
"#;

    assert_eq!(compile_and_run(source), 9);
}

#[test]
fn callable_supports_owned_string_argument() {
    let source = r#"
fn message_length(message: String) -> i32 {
  ret message.len()
}

fn main() -> i32 {
  let function = message_length
  let message = String::from_str("forty-two")
  ret function(message)
}
"#;

    assert_eq!(compile_and_run(source), 9);
}

#[test]
fn extern_function_item_works_in_jit_and_object_codegen() {
    extern "C" fn host_add_two(value: i32) -> i32 {
        value + 2
    }

    let output = compile(
        r#"
extern "C" fn host_add_two(value: i32) -> i32;
fn main() -> i32 {
  let function = host_add_two
  ret function(40)
}
"#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mut codegen = CodegenContext::new("callable_extern_test").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    let symbols = HashMap::from([("host_add_two".to_string(), host_add_two as *const () as u64)]);
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        42
    );

    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("callable_extern.o");
    codegen.emit_object_file(&object).unwrap();
    assert!(object.is_file());
}

#[test]
fn callable_wrong_arity_is_rejected() {
    let diagnostics = messages(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn main() -> i32 {
  let function = identity
  ret function()
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("expects 1 arguments but got 0")),
        "diagnostics: {:?}",
        diagnostics
    );
}

#[test]
fn callable_wrong_argument_type_is_rejected() {
    let diagnostics = messages(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn main() -> i32 {
  let function = identity
  ret function(true)
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("has type 'bool', expected 'i32'")),
        "diagnostics: {:?}",
        diagnostics
    );
}

#[test]
fn non_callable_local_is_rejected() {
    let diagnostics = messages(
        r#"
fn main() -> i32 {
  let value: i32 = 1
  ret value()
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("is not callable")),
        "diagnostics: {:?}",
        diagnostics
    );
}

#[test]
fn fnonce_local_cannot_be_called_twice() {
    let diagnostics = messages(
        r#"
fn answer() -> i32 { ret 42 }
fn main() -> i32 {
  let function = answer
  let first = function()
  ret function()
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("use of moved value `function`")),
        "diagnostics: {:?}",
        diagnostics
    );
}

#[test]
fn function_item_signature_mismatch_is_rejected() {
    let diagnostics = messages(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn main() -> i32 {
  let function: FnOnce<bool, i32> = identity
  ret 0
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("expected 'FnOnce<bool, i32>'")),
        "diagnostics: {:?}",
        diagnostics
    );
}
