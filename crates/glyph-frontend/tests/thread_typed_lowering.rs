use glyph_core::mir::{MirInst, Rvalue};
use glyph_core::thread::canonical_thread_handle_type;
use glyph_core::types::Type;
use glyph_frontend::{FrontendOptions, compile_source};

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
}

fn rvalues(output: &glyph_frontend::FrontendOutput) -> impl Iterator<Item = &Rvalue> {
    output
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MirInst::Assign { value, .. } => Some(value),
            _ => None,
        })
}

#[test]
fn typed_spawn_and_join_specialize_from_the_callable_return() {
    let output = compile(
        r#"
import spawn from std/thread

fn answer() -> i32 { ret 42 }

fn main() -> i32 {
  let outcome = spawn(answer)
  ret match outcome {
    Ok(handle) => {
      let joined = handle.join()
      0
    },
    Err(_error) => 1,
  }
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| {
        matches!(value, Rvalue::ThreadSpawnResult { result_type, .. } if result_type == &Type::I32)
    }));
    assert!(rvalues(&output).any(|value| {
        matches!(value, Rvalue::ThreadJoinResult { result_type, .. } if result_type == &Type::I32)
    }));
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.locals)
            .any(|local| { local.ty.as_ref() == Some(&canonical_thread_handle_type(Type::I32)) })
    );
}

#[test]
fn typed_spawn_rejects_a_non_send_result_before_emitting_runtime_mir() {
    let output = compile(
        r#"
import spawn from std/thread

fn build_state() -> Shared<i32> { ret Shared::new(7) }

fn main() -> i32 {
  let outcome = spawn(build_state)
  ret 0
}
"#,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("spawn result")
                && message.contains("Shared<T>")
                && message.contains("cannot cross threads")
        }),
        "{messages:?}"
    );
    assert!(!rvalues(&output).any(|value| matches!(value, Rvalue::ThreadSpawnResult { .. })));
}
