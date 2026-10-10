#![cfg(feature = "codegen")]

use std::collections::HashMap;

use glyph_backend::codegen::CodegenContext;
use glyph_core::ast::Module;
use glyph_frontend::{FrontendOptions, FrontendOutput, compile_modules, lex, parse};

fn compile(sources: &[(&str, &str)]) -> FrontendOutput {
    let modules: HashMap<String, Module> = sources
        .iter()
        .map(|(id, source)| {
            let tokens = lex(source);
            assert!(tokens.diagnostics.is_empty(), "{:?}", tokens.diagnostics);
            let parsed = parse(&tokens.tokens, source);
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            ((*id).to_string(), parsed.module)
        })
        .collect();
    compile_modules(
        modules,
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
}

fn execute(sources: &[(&str, &str)]) -> i32 {
    let output = compile(sources);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let mut codegen = CodegenContext::new("module_scoped_functions").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.jit_execute_i32("main").unwrap()
}

#[test]
fn transitive_import_keeps_each_helper_in_its_own_module() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from b import b\nfn helper() -> i32 { ret 1 }\nfn main() -> i32 { ret helper() + b() }"
            ),
            ("b", "from a import helper\nfn b() -> i32 { ret helper() }"),
            ("a", "fn helper() -> i32 { ret 2 }"),
        ]),
        3
    );
}

#[test]
fn aliases_and_qualified_calls_select_the_correct_definition() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper as first\nfrom b import helper as second\nimport a\nimport b\nfn main() -> i32 { ret first() + second() + a::helper() + b::helper() }"
            ),
            ("a", "fn helper() -> i32 { ret 1 }"),
            ("b", "fn helper() -> i32 { ret 2 }"),
        ]),
        6
    );
}

#[test]
fn recursive_functions_call_their_own_module() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper as first\nfrom b import helper as second\nfn main() -> i32 { ret first(3) + second(3) }"
            ),
            (
                "a",
                "fn helper(n: i32) -> i32 { if n == 0 { ret 1 } else { ret helper(n - 1) + 1 } }"
            ),
            (
                "b",
                "fn helper(n: i32) -> i32 { if n == 0 { ret 2 } else { ret helper(n - 1) + 2 } }"
            ),
        ]),
        12
    );
}

#[test]
fn function_values_keep_their_module_identity() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper as first\nfrom b import helper as second\nfn main() -> i32 { let f: FnOnce<(), i32> = first\nlet g: FnOnce<(), i32> = second\nret f() + g() }"
            ),
            ("a", "fn helper() -> i32 { ret 1 }"),
            ("b", "fn helper() -> i32 { ret 2 }"),
        ]),
        3
    );
}

#[test]
fn closure_factories_and_string_literals_have_distinct_symbols() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper as first\nfrom b import helper as second\nfn main() -> i32 { let f = first()\nlet g = second()\nret f() + g() }"
            ),
            (
                "a",
                "fn helper() -> FnOnce<(), i32> { let text: String = \"a\"\nret () -> text.len() as i32 }"
            ),
            (
                "b",
                "fn helper() -> FnOnce<(), i32> { let text: String = \"bb\"\nret () -> text.len() as i32 }"
            ),
        ]),
        3
    );
}

#[test]
fn local_declaration_shadows_an_import_and_dependency_main_is_not_the_entry() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper\nfrom a import main as other_main\nfn helper() -> i32 { ret 1 }\nfn main() -> i32 { ret helper() + other_main() }"
            ),
            (
                "a",
                "fn helper(n: i32) -> i32 { ret n }\nfn main() -> i32 { ret helper(9) }"
            ),
        ]),
        10
    );
}

#[test]
fn glyph_function_can_share_a_name_with_an_extern() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import abs as c_abs\nfn abs(n: i32) -> i32 { ret n + 1 }\nfn main() -> i32 { ret abs(1) + c_abs(-1) }"
            ),
            ("a", "extern \"C\" fn abs(n: i32) -> i32;"),
        ]),
        3
    );
}

#[test]
fn colliding_functions_use_their_own_parameter_and_return_types() {
    assert_eq!(
        execute(&[
            (
                "main",
                "from a import helper as first\nfrom b import helper as second\nfn main() -> i32 { if second(true) { ret first(2) } else { ret 0 } }"
            ),
            ("a", "fn helper(n: i32) -> i32 { ret n + 1 }"),
            ("b", "fn helper(value: bool) -> bool { ret value }"),
        ]),
        3
    );
}

#[test]
fn local_function_shadows_a_selective_std_intrinsic_import() {
    assert_eq!(
        execute(&[(
            "main",
            "from std/thread import spawn\nfn spawn(n: i32) -> i32 { ret n + 1 }\nfn main() -> i32 { ret spawn(2) }"
        ),]),
        3
    );
}

#[test]
fn compatible_externs_deduplicate_and_conflicting_signatures_are_rejected() {
    let output = compile(&[
        (
            "main",
            "import a\nimport b\nfn main() -> i32 { ret a::abs(-3) + b::abs(-2) }",
        ),
        ("a", "extern \"C\" fn abs(n: i32) -> i32;"),
        ("b", "extern \"C\" fn abs(n: i32) -> i32;"),
    ]);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(
        output
            .mir
            .extern_functions
            .iter()
            .filter(|f| f.name == "abs")
            .count(),
        1
    );
    let mut codegen = CodegenContext::new("shared_extern").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(codegen.jit_execute_i32("main").unwrap(), 5);

    let output = compile(&[
        ("main", "import a\nimport b\nfn main() -> i32 { ret 0 }"),
        ("a", "extern \"C\" fn abs(n: i32) -> i32;"),
        ("b", "extern \"C\" fn abs(n: i64) -> i32;"),
    ]);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|d| d.message.contains("conflicting declarations"))
    );
}

#[test]
fn ambiguous_imports_and_duplicate_local_definitions_are_still_errors() {
    for main in [
        "from a import helper\nfrom b import helper\nfn main() -> i32 { ret helper() }",
        "import a\nimport b\nfn helper() -> i32 { ret 1 }\nfn helper() -> i32 { ret 2 }\nfn main() -> i32 { ret helper() }",
    ] {
        let output = compile(&[
            ("main", main),
            ("a", "fn helper() -> i32 { ret 1 }"),
            ("b", "fn helper() -> i32 { ret 2 }"),
        ]);
        assert!(!output.diagnostics.is_empty());
    }
}
