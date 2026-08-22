use glyph_core::mir::{MirInst, Rvalue};
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

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn function_items_and_callable_parameters_lower_without_rewriting_direct_calls() {
    let output = compile(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn apply(function: FnOnce<i32, i32>, value: i32) -> i32 {
  ret function(value)
}
fn main() -> i32 { ret apply(identity, 42) }
"#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
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
                    MirInst::Assign {
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
                    MirInst::Assign {
                        value: Rvalue::FunctionRef { name, .. },
                        ..
                    } if name == "identity"
                )
            })
    );
    assert!(
        main.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| {
                matches!(
                    inst,
                    MirInst::Assign {
                        value: Rvalue::Call { name, .. },
                        ..
                    } if name == "apply"
                )
            })
    );
}

#[test]
fn callable_wrong_arity_and_argument_type_are_rejected() {
    let arity = messages(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn main() -> i32 { let function = identity ret function() }
"#,
    );
    assert!(
        arity
            .iter()
            .any(|message| message.contains("expects 1 arguments but got 0")),
        "diagnostics: {:?}",
        arity
    );

    let argument = messages(
        r#"
fn identity(value: i32) -> i32 { ret value }
fn main() -> i32 { let function = identity ret function(true) }
"#,
    );
    assert!(
        argument
            .iter()
            .any(|message| message.contains("has type 'bool', expected 'i32'")),
        "diagnostics: {:?}",
        argument
    );
}

#[test]
fn non_callable_and_second_fnonce_call_are_rejected() {
    let non_callable = messages(
        r#"
fn main() -> i32 { let value: i32 = 1 ret value() }
"#,
    );
    assert!(
        non_callable
            .iter()
            .any(|message| message.contains("is not callable")),
        "diagnostics: {:?}",
        non_callable
    );

    let called_twice = messages(
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
        called_twice
            .iter()
            .any(|message| message.contains("use of moved value `function`")),
        "diagnostics: {:?}",
        called_twice
    );
}

#[test]
fn incompatible_function_item_signature_is_rejected() {
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

#[test]
fn callable_return_type_mismatch_is_rejected() {
    let diagnostics = messages(
        r#"
fn predicate() -> bool { ret true }
fn main() -> i32 {
  let function = predicate
  let value: i32 = function()
  ret value
}
"#,
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("returns 'bool', but 'i32' is required")),
        "diagnostics: {:?}",
        diagnostics
    );
}
