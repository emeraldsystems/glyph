use glyph_core::mir::{MirInst, Rvalue};
use glyph_core::types::Type;
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
        .filter_map(|instruction| match instruction {
            MirInst::Assign { value, .. } => Some(value),
            _ => None,
        })
}

#[test]
fn canonical_arc_new_clone_and_borrow_lower_to_owned_mir() {
    let output = compile(
        r#"
import Arc from std/sync

fn observe(value: i32) -> i32 { ret value }

fn main() -> i32 {
  let original = Arc::new(42)
  let cloned = original.clone()
  let view = cloned.borrow()
  ret observe(view)
}
"#,
        true,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::ArcNew { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::ArcClone { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::ArcBorrow { .. })));
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.locals)
            .any(|local| local.ty.as_ref() == Some(&Type::arc(Type::I32)))
    );
}

#[test]
fn same_named_user_arc_never_acquires_compiler_intrinsics() {
    let output = compile(
        r#"
struct Arc<T> { value: T }

fn main() -> i32 {
  let value = Arc::new(42)
  ret 0
}
"#,
        false,
    );
    assert!(
        !rvalues(&output).any(|value| matches!(
            value,
            Rvalue::ArcNew { .. } | Rvalue::ArcClone { .. } | Rvalue::ArcBorrow { .. }
        )),
        "user Arc must retain ordinary semantics"
    );
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unknown function 'Arc::new'")),
        "diagnostics: {:?}",
        output.diagnostics
    );
}

#[test]
fn moving_arc_consumes_one_owner_but_explicit_clone_does_not() {
    let moved = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let original = Arc::new(1)
  let transferred = original
  let invalid = original.clone()
  ret 0
}
"#,
        true,
    );
    assert!(
        moved
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("use of moved value `original`")),
        "diagnostics: {:?}",
        moved.diagnostics
    );

    let cloned = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let original = Arc::new(1)
  let duplicate = original.clone()
  let view = original.borrow()
  ret 0
}
"#,
        true,
    );
    assert!(cloned.diagnostics.is_empty(), "{:?}", cloned.diagnostics);
}

#[test]
fn arc_rejects_mutable_access_borrowed_payloads_and_borrow_returns() {
    let mutable = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let value = Arc::new(1)
  let invalid = value.borrow_mut()
  ret 0
}
"#,
        true,
    );
    assert!(
        mutable.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("provides immutable access only")),
        "diagnostics: {:?}",
        mutable.diagnostics
    );

    let borrowed_payload = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let local = 1
  let invalid = Arc::new(&local)
  ret 0
}
"#,
        true,
    );
    assert!(
        borrowed_payload
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("cannot own a borrowed reference")),
        "diagnostics: {:?}",
        borrowed_payload.diagnostics
    );

    let returned_borrow = compile(
        r#"
import Arc from std/sync

fn leak() -> &i32 {
  let value = Arc::new(1)
  ret value.borrow()
}
"#,
        true,
    );
    assert!(
        returned_borrow
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("borrowed references cannot escape through return")),
        "diagnostics: {:?}",
        returned_borrow.diagnostics
    );
}

#[test]
fn spawn_rejects_arc_of_shared_with_a_nested_constraint_trace() {
    let output = compile(
        r#"
import Arc from std/sync
import spawn from std/thread

fn main() -> i32 {
  let state = Arc::new(Shared::new(7))
  let task: FnOnce<(), ()> = move () -> {
    let copy = state.clone()
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
                && message.contains("Arc<T>")
                && message.contains("Shared<T>")
        }),
        "{messages:?}"
    );
}

#[test]
fn spawn_rejects_arc_of_raw_pointer_and_nested_non_send_fields() {
    let raw = compile(
        r#"
import Arc from std/sync
import spawn from std/thread

fn main() -> i32 {
  let owned = Own::new(7)
  let raw: RawPtr<i32> = owned.into_raw()
  let state = Arc::new(raw)
  let task: FnOnce<(), ()> = move () -> {
    let copy = state.clone()
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let raw_messages = raw
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        raw_messages.iter().any(|message| {
            message.contains("spawn task.capture")
                && message.contains("Arc<T>")
                && message.contains("raw pointers")
        }),
        "{raw_messages:?}"
    );

    let nested = compile(
        r#"
import Arc from std/sync
import spawn from std/thread

struct State { local: Shared<i32> }

fn main() -> i32 {
  let state = Arc::new(State { local: Shared::new(7) })
  let task: FnOnce<(), ()> = move () -> {
    let copy = state.clone()
  }
  let outcome = spawn(task)
  ret 0
}
"#,
        true,
    );
    let nested_messages = nested
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        nested_messages.iter().any(|message| {
            message.contains("spawn task.capture")
                && message.contains("`state`.value.local")
                && message.contains("Arc<T>")
                && message.contains("Shared<T>")
        }),
        "{nested_messages:?}"
    );
}

#[test]
fn arc_owner_cannot_move_reassign_or_drop_while_a_lexical_loan_is_live() {
    let moved = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let owner = Arc::new(AtomicI32::new(7))
  let view = owner.borrow()
  let moved = owner
  ret view.load()
}
"#,
        true,
    );
    assert!(
        moved.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("cannot move Arc owner `owner` while an Arc::borrow() reference is active")),
        "{:?}",
        moved.diagnostics
    );

    let reassigned = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let mut owner = Arc::new(AtomicI32::new(7))
  let view = owner.borrow()
  owner = Arc::new(AtomicI32::new(8))
  ret view.load()
}
"#,
        true,
    );
    assert!(
        reassigned
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(
                "cannot reassign Arc owner `owner` while an Arc::borrow() reference is active"
            )),
        "{:?}",
        reassigned.diagnostics
    );

    let escaped_scope = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let view: &AtomicI32 = {
    let owner = Arc::new(AtomicI32::new(7))
    owner.borrow()
  }
  ret view.load()
}
"#,
        true,
    );
    let messages = escaped_scope
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| message.contains(
            "Arc::borrow() reference cannot escape the lexical scope where it was created"
        )),
        "{messages:?}"
    );
}

#[test]
fn arc_loan_ends_at_nested_scope_exit_before_owner_move() {
    let output = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let owner = Arc::new(AtomicI32::new(7))
  {
    let view = owner.borrow()
    let observed = view.load()
  }
  {
    owner.borrow()
  }
  let moved = owner
  ret 0
}
"#,
        true,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn arc_borrows_cannot_be_stored_in_aggregates_containers_or_closures() {
    let aggregate = compile(
        r#"
import Arc from std/sync

struct Holder { value: &AtomicI32 }

enum MaybeRef {
  Ref(&AtomicI32)
  Empty
}

fn main() -> i32 {
  let owner = Arc::new(AtomicI32::new(7))
  let view = owner.borrow()
  let holder = Holder { value: view }
  let tuple = (view,)
  let array = [view]
  let tagged: MaybeRef = MaybeRef::Ref(view)
  ret 0
}
"#,
        true,
    );
    let aggregate_messages = aggregate
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        aggregate_messages
            .iter()
            .filter(|message| message.contains("Arc::borrow() reference cannot be stored"))
            .count()
            >= 4,
        "{aggregate_messages:?}"
    );

    let containers = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let owner = Arc::new(AtomicI32::new(7))
  let view = owner.borrow()
  let values: Vec<&AtomicI32> = Vec::new()
  values.push(view)
  let indexed: Map<i32, &AtomicI32> = Map::new()
  indexed.add(1, view)
  let boxed = Own::new(view)
  let shared = Shared::new(view)
  let callback: FnOnce<(), ()> = move () -> {
    let observed = view.load()
  }
  ret 0
}
"#,
        true,
    );
    let container_messages = containers
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        container_messages
            .iter()
            .any(|message| message.contains("stored in a Vec")),
        "{container_messages:?}"
    );
    assert!(
        container_messages
            .iter()
            .any(|message| message.contains("stored in a Map")),
        "{container_messages:?}"
    );
    assert!(
        container_messages
            .iter()
            .any(|message| message.contains("stored in an Own allocation")),
        "{container_messages:?}"
    );
    assert!(
        container_messages
            .iter()
            .any(|message| message.contains("stored in a Shared allocation")),
        "{container_messages:?}"
    );
    assert!(
        container_messages.iter().any(|message| {
            message.contains("cannot be captured by a closure")
                || message.contains("borrowed capture `view` cannot escape")
        }),
        "{container_messages:?}"
    );

    let captured_owner = compile(
        r#"
import Arc from std/sync

fn main() -> i32 {
  let owner = Arc::new(AtomicI32::new(7))
  let view = owner.borrow()
  let callback: FnOnce<(), ()> = move () -> {
    let clone = owner.clone()
  }
  ret view.load()
}
"#,
        true,
    );
    assert!(
        captured_owner.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("cannot move into a closure Arc owner `owner` while an Arc::borrow() reference is active")),
        "{:?}",
        captured_owner.diagnostics
    );
}
