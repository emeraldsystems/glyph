use glyph_frontend::{FrontendOptions, compile_source};

fn messages(source: &str) -> Vec<String> {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
    .diagnostics
    .into_iter()
    .map(|diagnostic| diagnostic.message)
    .collect()
}

fn assert_message(source: &str, needle: &str) {
    let diagnostics = messages(source);
    assert!(
        diagnostics.iter().any(|message| message.contains(needle)),
        "expected {needle:?}, got {diagnostics:?}"
    );
}

#[test]
fn mutable_borrow_requires_a_mutable_binding() {
    assert_message(
        r#"
fn main() {
  let value: i32 = 1
  let reference: &mut i32 = &mut value
}
"#,
        "cannot mutably borrow immutable variable `value`",
    );
}

#[test]
fn shared_and_mutable_loans_conflict_in_both_orders() {
    assert_message(
        r#"
fn main() {
  let mut value: i32 = 1
  let shared: &i32 = &value
  let exclusive: &mut i32 = &mut value
}
"#,
        "cannot mutably borrow `value` while a shared loan is active",
    );
    assert_message(
        r#"
fn main() {
  let mut value: i32 = 1
  let exclusive: &mut i32 = &mut value
  let shared: &i32 = &value
}
"#,
        "cannot immutably borrow `value` while an exclusive loan is active",
    );
}

#[test]
fn owner_cannot_move_or_reassign_until_reference_scope_ends() {
    assert_message(
        r#"
fn consume(value: String) {}
fn main() {
  let owner: String = String::from_str("hello")
  let view: &String = &owner
  consume(owner)
}
"#,
        "cannot move `owner` while a shared loan is active",
    );
    assert_message(
        r#"
fn main() {
  let mut value: i32 = 1
  let view: &i32 = &value
  value = 2
}
"#,
        "cannot reassign `value` while a shared loan is active",
    );

    let valid = messages(
        r#"
fn consume(value: String) {}
fn main() {
  let owner: String = String::from_str("hello")
  {
    let view: &String = &owner
  }
  consume(owner)
}
"#,
    );
    assert!(valid.is_empty(), "{valid:?}");
}

#[test]
fn shared_loan_provenance_survives_reference_propagation() {
    assert_message(
        r#"
fn consume(value: String) {}
fn main() {
  let owner: String = String::from_str("hello")
  let first: &String = &owner
  let second: &String = first
  consume(owner)
}
"#,
        "cannot move `owner` while a shared loan is active",
    );
}
