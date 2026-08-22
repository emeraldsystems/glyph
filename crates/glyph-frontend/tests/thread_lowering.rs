use glyph_core::mir::{MirInst, Rvalue};
use glyph_core::thread::canonical_unit_handle_type;
use glyph_frontend::{FrontendOptions, compile_source};

fn compile(source: &str, include_std: bool) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std,
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
fn canonical_spawn_lowers_only_after_resolver_identity_and_drops_unused_result() {
    let output = compile(
        r#"
import spawn from std/thread

fn worker() {}

fn main() -> i32 {
  let outcome = spawn(worker)
  ret 0
}
"#,
        true,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::ThreadSpawnUnit { .. })));
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.locals)
            .any(|local| local.ty.as_ref() == Some(&canonical_unit_handle_type()))
    );
    assert!(output.mir.functions.iter().any(|function| {
        function.blocks.iter().flat_map(|block| &block.insts).any(|inst| {
            matches!(inst, MirInst::Drop(local) if function.locals[local.0 as usize].name.as_deref() == Some("outcome"))
        })
    }));
}

#[test]
fn user_defined_spawn_and_join_handle_never_gain_thread_intrinsics() {
    let output = compile(
        r#"
struct JoinHandle<T> { value: T }

fn worker() {}
fn spawn(task: FnOnce<(), ()>) -> i32 { ret 7 }

fn main() -> i32 { ret spawn(worker) }
"#,
        false,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(!rvalues(&output).any(|value| matches!(value, Rvalue::ThreadSpawnUnit { .. })));
    assert!(
        rvalues(&output).any(|value| matches!(value, Rvalue::Call { name, .. } if name == "spawn"))
    );
    assert!(
        !output
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.locals)
            .any(|local| { local.ty.as_ref() == Some(&canonical_unit_handle_type()) })
    );
}

#[test]
fn spawn_consumes_callable_and_rejects_source_reuse() {
    let output = compile(
        r#"
import spawn from std/thread

fn worker() {}

fn main() -> i32 {
  let task: FnOnce<(), ()> = worker
  let outcome = spawn(task)
  task()
  ret 0
}
"#,
        true,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("use of moved value `task`")),
        "{messages:?}"
    );
}

#[test]
fn join_consumes_the_canonical_handle_and_second_use_is_rejected() {
    let output = compile(
        r#"
import spawn from std/thread

fn worker() {}

fn main() -> i32 {
  let outcome = spawn(worker)
  ret match outcome {
    Ok(handle) => {
      let joined = handle.join()
      let detached = handle.detach()
      0
    },
    Err(_error) => 1,
  }
}
"#,
        true,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("use of moved value `handle`")),
        "{messages:?}"
    );
}

#[test]
fn spawn_rejects_borrowed_capture_at_the_transfer_site() {
    let output = compile(
        r#"
import spawn from std/thread

fn main() -> i32 {
  let value: i32 = 7
  let borrowed: &i32 = &value
  let task: FnOnce<(), ()> = move () -> {
    let observed = borrowed
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("borrowed capture `borrowed` cannot escape through a transfer site")
        }),
        "{messages:?}"
    );
}

#[test]
fn spawn_rejects_shared_capture_with_capture_path_trace() {
    let output = compile(
        r#"
import spawn from std/thread

fn main() -> i32 {
  let state: Shared<i32> = Shared::new(7)
  let task: FnOnce<(), ()> = move () -> {
    let alias = state.clone()
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("spawn task.capture")
                && message.contains("`state`")
                && message.contains("Shared<T>")
                && message.contains("cannot cross threads")
        }),
        "{messages:?}"
    );
}

#[test]
fn callable_provenance_survives_a_function_return_and_local_move() {
    let output = compile(
        r#"
import spawn from std/thread

fn make_task() -> FnOnce<(), ()> {
  let message: String = String::from_str("returned")
  ret move () -> {
    let length = message.len()
  }
}

fn main() -> i32 {
  let returned: FnOnce<(), ()> = make_task()
  let task: FnOnce<(), ()> = returned
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::ThreadSpawnUnit { .. })));
}

#[test]
fn returned_callable_keeps_non_send_capture_certificate() {
    let output = compile(
        r#"
import spawn from std/thread

fn make_task() -> FnOnce<(), ()> {
  let state: Shared<i32> = Shared::new(7)
  ret move () -> {
    let alias = state.clone()
  }
}

fn main() -> i32 {
  let task: FnOnce<(), ()> = make_task()
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("`state`")
                && message.contains("Shared<T>")
                && !message.contains("provenance is unknown")
        }),
        "{messages:?}"
    );
}

#[test]
fn audited_runtime_policy_survives_nested_user_aggregates() {
    let denied = compile(
        r#"
import spawn from std/thread
from std/audio import AudioOut, WavWriter

struct Studio {
  output: AudioOut,
  writer: WavWriter,
}

fn main() -> i32 {
  let studio = Studio {
    output: AudioOut { handle: 0 },
    writer: WavWriter { handle: 0 },
  }
  let task: FnOnce<(), ()> = move () -> {
    let owned = studio
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let messages = denied
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("spawn task.capture")
                && message.contains("`studio`")
                && message.contains(".output")
                && message.contains("live audio devices are engine-thread-affine")
        }),
        "{messages:?}"
    );

    let accepted = compile(
        r#"
import Arc from std/sync
import spawn from std/thread
from std/audio import WavWriter

struct OfflineRender {
  writer: WavWriter,
  sample_count: Arc<i32>,
}

fn main() -> i32 {
  let render = OfflineRender {
    writer: WavWriter { handle: 0 },
    sample_count: Arc::new(48000),
  }
  let task: FnOnce<(), ()> = move () -> {
    let owned = render
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    assert!(
        accepted.diagnostics.is_empty(),
        "{:?}",
        accepted.diagnostics
    );
    assert!(rvalues(&accepted).any(|value| matches!(value, Rvalue::ThreadSpawnUnit { .. })));

    let rejected_arc = compile(
        r#"
import Arc from std/sync
import spawn from std/thread

struct SharedState {
  value: Arc<Shared<i32>>,
}

fn main() -> i32 {
  let state = SharedState { value: Arc::new(Shared::new(7)) }
  let task: FnOnce<(), ()> = move () -> {
    let owned = state
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let messages = rejected_arc
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| {
            message.contains("`state`")
                && message.contains(".value")
                && message.contains("Arc<T>")
                && message.contains("Shared<T>")
        }),
        "{messages:?}"
    );
}
