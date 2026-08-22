use glyph_core::mir::{BorrowKind, MirInst, Rvalue};
use glyph_core::types::{BorrowedCallableKind, Type};
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
fn fn_callback_captures_shared_and_is_called_repeatedly() {
    let output = compile(
        r#"
fn apply_twice(function: Fn<i32, i32>, value: i32) -> i32 {
  let first: i32 = function(value)
  ret first + function(value)
}

fn main() -> i32 {
  let base: i32 = 1
  let add: Fn<i32, i32> = (value: i32) -> base + value
  ret apply_twice(add, 20)
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
                        value: Rvalue::MakeBorrowedClosure { signature, captures, .. },
                        ..
                    } if matches!(
                        signature,
                        Type::BorrowedFunction {
                            kind: BorrowedCallableKind::Fn,
                            ..
                        }
                    ) && captures.len() == 1 && captures[0].borrow == BorrowKind::Shared
                )
            })
    );

    let apply = output
        .mir
        .functions
        .iter()
        .find(|function| function.name == "apply_twice")
        .unwrap();
    assert_eq!(
        apply
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(
                inst,
                MirInst::Assign {
                    value: Rvalue::CallIndirectShared { .. },
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn fnmut_callback_captures_mutably_and_is_called_repeatedly() {
    let output = compile(
        r#"
struct Counter { value: i32 }

fn call_twice(function: FnMut<(), i32>) -> i32 {
  let first: i32 = function()
  ret first + function()
}

fn main() -> i32 {
  let mut state: Counter = Counter { value: 0 }
  let mut next: FnMut<(), i32> = () -> {
    state.value = state.value + 1
    state.value
  }
  ret call_twice(next)
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
                        value: Rvalue::MakeBorrowedClosure { signature, captures, .. },
                        ..
                    } if matches!(
                        signature,
                        Type::BorrowedFunction {
                            kind: BorrowedCallableKind::FnMut,
                            ..
                        }
                    ) && captures.len() == 1 && captures[0].borrow == BorrowKind::Mutable
                )
            })
    );

    let call_twice = output
        .mir
        .functions
        .iter()
        .find(|function| function.name == "call_twice")
        .unwrap();
    assert_eq!(
        call_twice
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(
                inst,
                MirInst::Assign {
                    value: Rvalue::CallIndirectMut { .. },
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn borrowed_callback_capability_and_escape_errors_are_explicit() {
    let mutating_fn = messages(
        r#"
struct Counter { value: i32 }
fn main() {
  let mut state: Counter = Counter { value: 0 }
  let callback: Fn<(), i32> = () -> {
    state.value = 1
    state.value
  }
}
"#,
    );
    assert!(
        mutating_fn
            .iter()
            .any(|message| message.contains("Fn closure cannot mutate capture `state`")),
        "{mutating_fn:?}"
    );

    let returned = messages(
        r#"
fn invalid() -> Fn<(), i32> {
  let value: i32 = 1
  ret () -> value
}
"#,
    );
    assert!(
        returned
            .iter()
            .any(|message| message.contains("borrowed callable") && message.contains("return")),
        "{returned:?}"
    );

    let move_closure = messages(
        r#"
fn main() {
  let value: i32 = 1
  let callback: Fn<(), i32> = move () -> value
}
"#,
    );
    assert!(
        move_closure
            .iter()
            .any(|message| message.contains("move closure") && message.contains("FnOnce")),
        "{move_closure:?}"
    );
}

#[test]
fn borrowed_callbacks_hold_loans_and_cannot_enter_storage() {
    let moved_owner = messages(
        r#"
fn consume(value: String) {}
fn main() {
  let owner: String = String::from_str("hello")
  let callback: Fn<(), usize> = () -> owner.len()
  consume(owner)
}
"#,
    );
    assert!(
        moved_owner
            .iter()
            .any(|message| message.contains("cannot move `owner` while a shared loan is active")),
        "{moved_owner:?}"
    );

    let mutable_owner_access = messages(
        r#"
struct Counter { value: i32 }
fn main() {
  let mut state: Counter = Counter { value: 0 }
  let mut callback: FnMut<(), i32> = () -> {
    state.value = state.value + 1
    state.value
  }
  let invalid: i32 = state.value
}
"#,
    );
    assert!(
        mutable_owner_access
            .iter()
            .any(|message| message.contains("cannot use `state` while an exclusive loan is active")),
        "{mutable_owner_access:?}"
    );

    let stored = messages(
        r#"
struct Holder { callback: Fn<(), i32> }
fn main() {
  let value: i32 = 1
  let callback: Fn<(), i32> = () -> value
  let holder: Holder = Holder { callback: callback }
}
"#,
    );
    assert!(
        stored.iter().any(
            |message| message.contains("borrowed callable") && message.contains("struct field")
        ),
        "{stored:?}"
    );
}

#[test]
fn fnmut_carriers_cannot_be_aliased_or_overlap_on_one_owner() {
    let alias = messages(
        r#"
struct Counter { value: i32 }
fn main() {
  let mut state: Counter = Counter { value: 0 }
  let mut callback: FnMut<(), i32> = () -> {
    state.value = 1
    state.value
  }
  let alias: FnMut<(), i32> = callback
}
"#,
    );
    assert!(
        alias
            .iter()
            .any(|message| message.contains("FnMut") && message.contains("alias")),
        "{alias:?}"
    );

    let overlap = messages(
        r#"
struct Counter { value: i32 }
fn main() {
  let mut state: Counter = Counter { value: 0 }
  let mut first: FnMut<(), i32> = () -> {
    state.value = 1
    state.value
  }
  let mut second: FnMut<(), i32> = () -> {
    state.value = 2
    state.value
  }
}
"#,
    );
    assert!(
        overlap
            .iter()
            .any(|message| message.contains("cannot mutably borrow `state`")
                && message.contains("exclusive loan")),
        "{overlap:?}"
    );

    let duplicate_argument = messages(
        r#"
fn use_both(first: FnMut<(), i32>, second: FnMut<(), i32>) -> i32 {
  ret first() + second()
}
fn invalid(function: FnMut<(), i32>) -> i32 {
  ret use_both(function, function)
}
"#,
    );
    assert!(
        duplicate_argument
            .iter()
            .any(|message| message.contains("FnMut") && message.contains("multiple arguments")),
        "{duplicate_argument:?}"
    );
}
