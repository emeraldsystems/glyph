use glyph_core::mir::{CaptureTransfer, LocalId, MirInst, Rvalue};
use glyph_core::types::Type;
use glyph_frontend::{FrontendOptions, compile_source, lex, lower_module, parse, resolve_types};

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
}

fn lower_despite_diagnostics(
    source: &str,
) -> (
    glyph_core::mir::MirModule,
    Vec<glyph_core::diag::Diagnostic>,
) {
    let lexed = lex(source);
    assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
    let parsed = parse(&lexed.tokens, source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let (resolver, resolve_diagnostics) = resolve_types(&parsed.module);
    assert!(resolve_diagnostics.is_empty(), "{:?}", resolve_diagnostics);
    lower_module(&parsed.module, &resolver)
}

fn make_closures(function: &glyph_core::mir::MirFunction) -> Vec<&Rvalue> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MirInst::Assign {
                value: value @ Rvalue::MakeClosure { .. },
                ..
            } => Some(value),
            _ => None,
        })
        .collect()
}

#[test]
fn captured_copy_is_lifted_before_declared_parameters() {
    let output = compile(
        r#"
fn main() -> i32 {
  let base: i32 = 40
  let add: FnOnce<i32, i32> = (value: i32) -> base + value
  ret add(2)
}
"#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    let main = &output.mir.functions[0];
    let closures = make_closures(main);
    let Rvalue::MakeClosure {
        function,
        signature,
        captures,
    } = closures[0]
    else {
        unreachable!()
    };
    assert_eq!(function, "main::__glyph_closure_0");
    assert_eq!(
        signature,
        &Type::Function {
            params: vec![Type::I32],
            ret: Box::new(Type::I32),
        }
    );
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].name, "base");
    assert_eq!(captures[0].transfer, CaptureTransfer::Copy);

    let lifted = &output.mir.functions[1];
    assert_eq!(lifted.name, *function);
    assert_eq!(lifted.params, vec![LocalId(0), LocalId(1)]);
    assert_eq!(lifted.locals[0].name.as_deref(), Some("base"));
    assert_eq!(lifted.locals[1].name.as_deref(), Some("value"));
}

#[test]
fn empty_and_nested_closures_have_stable_lexical_names_and_order() {
    let empty = compile(
        r#"
fn main() -> i32 {
  let answer: FnOnce<(), i32> = () -> 42
  ret answer()
}
"#,
    );
    assert!(empty.diagnostics.is_empty(), "{:?}", empty.diagnostics);
    let Rvalue::MakeClosure { captures, .. } = make_closures(&empty.mir.functions[0])[0] else {
        unreachable!()
    };
    assert!(captures.is_empty());

    let nested = compile(
        r#"
fn main() -> i32 {
  let base: i32 = 40
  let outer: FnOnce<i32, FnOnce<i32, i32>> =
    (left: i32) -> (right: i32) -> base + left + right
  let inner: FnOnce<i32, i32> = outer(1)
  ret inner(1)
}
"#,
    );
    assert!(
        nested.diagnostics.is_empty(),
        "diagnostics: {:?}",
        nested.diagnostics
    );
    assert_eq!(
        nested
            .mir
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<Vec<_>>(),
        vec!["main", "main::__glyph_closure_0", "main::__glyph_closure_1"]
    );
    let inner = &nested.mir.functions[2];
    assert_eq!(inner.locals[0].name.as_deref(), Some("base"));
    assert_eq!(inner.locals[1].name.as_deref(), Some("left"));
    assert_eq!(inner.locals[2].name.as_deref(), Some("right"));
}

#[test]
fn shadowed_capture_names_map_to_the_exact_declarations() {
    let output = compile(
        r#"
fn main() -> i32 {
  let value: i32 = 1
  let first: FnOnce<(), i32> = () -> value
  if true {
    let value: i32 = 2
    let second: FnOnce<(), i32> = () -> value
    let ignored: i32 = second()
  }
  ret first()
}
"#,
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let main = &output.mir.functions[0];
    let closures = make_closures(main);
    assert_eq!(closures.len(), 2);
    let capture_local = |value: &Rvalue| match value {
        Rvalue::MakeClosure { captures, .. } => captures[0].local,
        _ => unreachable!(),
    };
    let outer = capture_local(closures[0]);
    let inner = capture_local(closures[1]);
    assert_ne!(outer, inner);
    assert_eq!(main.locals[outer.0 as usize].name.as_deref(), Some("value"));
    assert_eq!(main.locals[inner.0 as usize].name.as_deref(), Some("value"));
}

#[test]
fn owned_capture_moves_and_analysis_errors_emit_no_closure_mir() {
    let moved = compile(
        r#"
fn main() -> usize {
  let text: String = String::from_str("hello")
  let callback: FnOnce<(), i32> = move () -> 5
  ret text.len()
}
"#,
    );
    // An unused name is not a capture, even for `move`; make the ownership
    // edge explicit in a second program.
    assert!(moved.diagnostics.is_empty(), "{:?}", moved.diagnostics);

    let moved = compile(
        r#"
fn main() -> usize {
  let text: String = String::from_str("hello")
  let callback: FnOnce<(), usize> = move () -> text.len()
  ret callback()
}
"#,
    );
    assert!(moved.diagnostics.is_empty(), "{:?}", moved.diagnostics);
    let Rvalue::MakeClosure { captures, .. } = make_closures(&moved.mir.functions[0])[0] else {
        unreachable!()
    };
    assert_eq!(captures[0].ty, Type::String);
    assert_eq!(captures[0].transfer, CaptureTransfer::Move);

    let invalid_source = r#"
fn main() -> usize {
  let text: String = String::from_str("hello")
  let callback: FnOnce<(), usize> = move () -> text.len()
  let again: usize = text.len()
  ret callback()
}
"#;
    let invalid = compile(invalid_source);
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("use of moved value `text`") })
    );
    let (invalid_mir, invalid_lower_diagnostics) = lower_despite_diagnostics(invalid_source);
    assert!(
        invalid_lower_diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("use of moved value `text`") })
    );
    assert!(make_closures(&invalid_mir.functions[0]).is_empty());
    assert_eq!(invalid_mir.functions.len(), 1);
}

#[test]
fn unknown_capture_layout_is_rejected_instead_of_becoming_nop() {
    let source = r#"
fn main() -> i32 {
  let mystery
  let callback: FnOnce<(), i32> = () -> mystery
  ret callback()
}
"#;
    let (mir, diagnostics) = lower_despite_diagnostics(source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("runtime layout is unknown") })
    );
    assert!(make_closures(&mir.functions[0]).is_empty());
}
