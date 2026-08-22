use glyph_core::mir::{MirInst, Rvalue};
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

fn messages(output: &glyph_frontend::FrontendOutput) -> Vec<&str> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

fn rvalues(output: &glyph_frontend::FrontendOutput) -> impl Iterator<Item = &Rvalue> {
    output
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|instruction| match instruction {
            MirInst::Assign { value, .. } => Some(value),
            _ => None,
        })
}

#[test]
fn canonical_mutex_lock_try_lock_and_guard_borrow_lower_to_typed_mir() {
    let output = compile(
        r#"
import Mutex from std/sync

fn main() -> i32 {
  let state = Mutex::new(AtomicI32::new(41))
  {
    let guard = state.lock()
    let value = guard.borrow_mut()
    let previous = value.fetch_add(1)
  }
  match state.try_lock() {
    Some(guard) => {
      let value = guard.borrow()
      value.load()
    },
    None => 0,
  }
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::MutexNew { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::MutexLock { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::MutexTryLock { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::MutexGuardBorrow { .. })));
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.locals)
            .any(|local| local.ty.as_ref()
                == Some(&Type::mutex(Type::Atomic(
                    glyph_core::atomic::AtomicScalar::I32
                ))))
    );
}

#[test]
fn same_named_user_mutex_never_selects_intrinsic_lowering() {
    let output = compile_source(
        r#"
struct Mutex<T> { value: T }
fn main() -> i32 {
  let state = Mutex::new(1)
  ret 0
}
"#,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(!rvalues(&output).any(|value| matches!(value, Rvalue::MutexNew { .. })));
    assert!(
        messages(&output)
            .iter()
            .any(|message| message.contains("unknown function 'Mutex::new'"))
    );
}

#[test]
fn guard_and_guard_borrows_cannot_escape_or_enter_storage() {
    for (source, expected) in [
        (
            r#"
import Mutex from std/sync
import MutexGuard from std/sync
fn leak() -> MutexGuard<i32> {
  let state = Mutex::new(1)
  ret state.lock()
}
"#,
            "cannot escape through a function return type",
        ),
        (
            r#"
import Mutex from std/sync
struct Holder { value: i32 }
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let tuple = (guard, 1)
  ret 0
}
"#,
            "cannot be stored in an aggregate",
        ),
        (
            r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let value = guard.borrow_mut()
  let tuple = (value, 1)
  ret 0
}
"#,
            "cannot be stored in an aggregate",
        ),
        (
            r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let task: FnOnce<(), ()> = move () -> { let moved = guard }
  ret 0
}
"#,
            "cannot be captured by a closure",
        ),
    ] {
        let output = compile(source);
        assert!(
            messages(&output)
                .iter()
                .any(|message| message.contains(expected)),
            "expected {expected:?}, diagnostics: {:?}",
            output.diagnostics
        );
    }
}

#[test]
fn live_guard_blocks_owner_move_and_guard_is_move_only() {
    let owner = compile(
        r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let moved = state
  ret 0
}
"#,
    );
    assert!(
        messages(&owner)
            .iter()
            .any(|message| message.contains("cannot move Mutex owner `state` while guard")),
        "{:?}",
        owner.diagnostics
    );

    let guard = compile(
        r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let moved = guard
  let invalid = guard.borrow_mut()
  ret 0
}
"#,
    );
    assert!(
        messages(&guard)
            .iter()
            .any(|message| message.contains("use of moved guard `guard`")),
        "{:?}",
        guard.diagnostics
    );
}

#[test]
fn mutex_thread_safety_depends_on_send_payload_and_guard_is_never_send() {
    let payload = compile(
        r#"
import Mutex from std/sync
import spawn from std/thread
fn main() -> i32 {
  let state = Mutex::new(Shared::new(1))
  let task: FnOnce<(), ()> = move () -> { let moved = state }
  let result = spawn(task)
  ret 0
}
"#,
    );
    assert!(
        messages(&payload).iter().any(|message| {
            message.contains("std::sync::Mutex") && message.contains("requires `T: Send`")
        }),
        "{:?}",
        payload.diagnostics
    );

    let guard = compile(
        r#"
import Mutex from std/sync
import spawn from std/thread
fn main() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  let task: FnOnce<(), ()> = move () -> { let moved = guard }
  let result = spawn(task)
  ret 0
}
"#,
    );
    assert!(
        messages(&guard).iter().any(|message| {
            message.contains("mutex guards are lexical") || message.contains("cannot be captured")
        }),
        "{:?}",
        guard.diagnostics
    );
}

#[test]
fn guard_drop_is_emitted_on_return_try_break_and_continue_edges() {
    let output = compile(
        r#"
import Mutex from std/sync

fn none() -> Option<i32> { ret None }

fn early_return() -> i32 {
  let state = Mutex::new(1)
  let guard = state.lock()
  ret 7
}

fn early_try() -> Option<i32> {
  let state = Mutex::new(1)
  let guard = state.lock()
  let value = none()?
  ret Some(value)
}

fn loop_edges() -> i32 {
  let state = Mutex::new(1)
  for i in 0..1 {
    let guard = state.lock()
    break
  }
  for i in 0..1 {
    let guard = state.lock()
    continue
  }
  ret 0
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    for name in ["early_return", "early_try", "loop_edges"] {
        let function = output
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap();
        let guard_locals = function
            .locals
            .iter()
            .enumerate()
            .filter_map(|(index, local)| {
                (local.ty.as_ref().is_some_and(Type::is_mutex_guard) && local.name.is_some())
                    .then_some(index as u32)
            })
            .collect::<Vec<_>>();
        assert!(!guard_locals.is_empty(), "{name} has no guard local");
        let drops = function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| {
                matches!(instruction, MirInst::Drop(local) if guard_locals.contains(&local.0))
            })
            .count();
        assert!(drops >= guard_locals.len(), "{name}: {function:#?}");
        for block in &function.blocks {
            if let Some(drop_index) = block.insts.iter().position(|instruction| {
                matches!(instruction, MirInst::Drop(local) if guard_locals.contains(&local.0))
            }) {
                assert!(
                    block.insts[drop_index + 1..].iter().any(|instruction| matches!(
                        instruction,
                        MirInst::Return(_) | MirInst::Goto(_) | MirInst::If { .. }
                    )),
                    "guard cleanup must precede the control-flow edge: {block:#?}"
                );
            }
        }
    }
}

#[test]
fn pending_try_lock_option_keeps_the_mutex_exclusively_loaned() {
    let output = compile(
        r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(1)
  let attempted = state.try_lock()
  let moved = state
  ret 0
}
"#,
    );
    assert!(
        messages(&output)
            .iter()
            .any(|message| message.contains("cannot move Mutex owner `state`")),
        "{:?}",
        output.diagnostics
    );
}
