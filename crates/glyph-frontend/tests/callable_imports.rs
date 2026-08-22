use std::collections::HashMap;

use glyph_core::{ast::Module, mir::Rvalue};
use glyph_frontend::{FrontendOptions, compile_modules, lex, parse};

fn parse_module(source: &str) -> Module {
    let lexed = lex(source);
    assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
    let parsed = parse(&lexed.tokens, source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    parsed.module
}

#[test]
fn imported_and_qualified_function_items_coerce_to_callables() {
    let functions = parse_module(
        r#"
fn increment(value: i32) -> i32 { ret value + 1 }
fn decrement(value: i32) -> i32 { ret value - 1 }
"#,
    );
    let main = parse_module(
        r#"
from functions import increment
import functions

fn main() -> i32 {
  let up: FnOnce<i32, i32> = increment
  let down: FnOnce<i32, i32> = functions::decrement
  let first = up(41)
  ret down(first + 1)
}
"#,
    );
    let output = compile_modules(
        HashMap::from([
            ("functions".to_string(), functions),
            ("main".to_string(), main),
        ]),
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let main = output
        .mir
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap();
    let references: Vec<&str> = main
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            glyph_core::mir::MirInst::Assign {
                value: Rvalue::FunctionRef { name, .. },
                ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(references.contains(&"increment"), "{:?}", references);
    assert!(references.contains(&"decrement"), "{:?}", references);
}
