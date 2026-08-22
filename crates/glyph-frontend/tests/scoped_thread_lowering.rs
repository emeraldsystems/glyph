use glyph_core::mir::{MirInst, Rvalue};
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

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn scope_spawn_and_unjoined_cleanup_lower_to_scoped_mir() {
    let output = compile(
        r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums

fn main() -> Result<i32, ThreadError> {
  let base: i32 = 40
  ret scope((thread_scope: Scope) -> {
    let local: i32 = 1
    let task: Fn<(), i32> = () -> base + local + 1
    let pending: Result<ScopedJoinHandle<i32>, ThreadError> = thread_scope.spawn(task)
    ret 42
  })
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

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
                        value: Rvalue::ThreadScopeCreate { .. },
                        ..
                    }
                )
            })
    );
    assert!(
        main.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst, MirInst::DropThreadScope(_)))
    );

    let callback = output
        .mir
        .functions
        .iter()
        .find(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| {
                    matches!(
                        inst,
                        MirInst::Assign {
                            value: Rvalue::ScopedThreadSpawnResult { .. },
                            ..
                        }
                    )
                })
        })
        .expect("lifted scope callback");
    for block in &callback.blocks {
        if let Some(return_index) = block
            .insts
            .iter()
            .position(|inst| matches!(inst, MirInst::Return(_)))
        {
            assert!(
                block.insts[..return_index]
                    .iter()
                    .any(|inst| matches!(inst, MirInst::DrainThreadScope(_))),
                "return without prior scope drain: {:?}",
                block.insts
            );
        }
    }
}

#[test]
fn scoped_callback_control_flow_drains_before_early_exits() {
    let output = compile(
        r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums

fn run() -> Result<Result<i32, ThreadError>, ThreadError> {
  ret scope((thread_scope: Scope) -> {
    let mut index: i32 = 0
    while index < 3 {
      let value: i32 = index
      let task: Fn<(), i32> = () -> value
      let pending: Result<ScopedJoinHandle<i32>, ThreadError> = thread_scope.spawn(task)
      index = index + 1
      if index == 1 { cont }
      if index == 2 { break }
    }
    ret Result::Ok(42)
  })
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let callback = output
        .mir
        .functions
        .iter()
        .find(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| matches!(inst, MirInst::DrainThreadScope(_)))
        })
        .expect("scope callback");
    assert!(
        callback
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(inst, MirInst::DrainThreadScope(_)))
            .count()
            >= 3
    );
}

#[test]
fn scoped_spawn_and_join_support_try_cleanup_paths() {
    let output = compile(
        r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums

fn main() -> Result<Result<i32, ThreadError>, ThreadError> {
  let value: i32 = 42
  ret scope((thread_scope: Scope) -> {
    let task: Fn<(), i32> = () -> value
    let handle: ScopedJoinHandle<i32> = thread_scope.spawn(task)?
    ret handle.join()
  })
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let callback = output
        .mir
        .functions
        .iter()
        .find(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| {
                    matches!(
                        inst,
                        MirInst::Assign {
                            value: Rvalue::ScopedThreadJoinResult { .. },
                            ..
                        }
                    )
                })
        })
        .expect("scope callback with explicit join");
    for block in &callback.blocks {
        if let Some(return_index) = block
            .insts
            .iter()
            .position(|inst| matches!(inst, MirInst::Return(_)))
        {
            assert!(
                block.insts[..return_index]
                    .iter()
                    .any(|inst| matches!(inst, MirInst::DrainThreadScope(_)))
            );
        }
    }
}

#[test]
fn scoped_tokens_and_fnmut_tasks_cannot_escape_or_overlap() {
    let detached = messages(
        r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums
fn invalid(handle: ScopedJoinHandle<()>) {
  handle.detach()
}
fn main() {}
"#,
    );
    assert!(
        detached
            .iter()
            .any(|message| message.contains("cannot be detached")),
        "{detached:?}"
    );

    let overlap = messages(
        r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums
fn main() -> Result<(), ThreadError> {
  ret scope((thread_scope: Scope) -> {
    let mut value: i32 = 0
    let task: FnMut<(), ()> = () -> { value = value + 1 }
    let first: Result<ScopedJoinHandle<()>, ThreadError> = thread_scope.spawn(task)
    let second: Result<ScopedJoinHandle<()>, ThreadError> = thread_scope.spawn(task)
  })
}
"#,
    );
    assert!(
        overlap
            .iter()
            .any(|message| message.contains("FnMut task") && message.contains("live scoped spawn")),
        "{overlap:?}"
    );
}
