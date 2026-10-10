use std::collections::HashMap;

use glyph_core::ast::Module;
use glyph_frontend::{FrontendOptions, compile_modules_with_aliases, lex, parse};

fn module(source: &str) -> Module {
    let tokens = lex(source);
    assert!(tokens.diagnostics.is_empty());
    let parsed = parse(&tokens.tokens, source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    parsed.module
}

#[test]
fn library_import_aliases_share_one_definition_even_when_function_names_collide() {
    let library = module("from dep/utils import helper\nfn answer() -> i32 { ret helper() }");
    let output = compile_modules_with_aliases(
        HashMap::from([
            (
                "main".into(),
                module(
                    "from dep import answer as first\nfrom dep/api import answer as second\nimport dep\nfn answer() -> i32 { ret 1 }\nfn main() -> i32 { let f: FnOnce<(), i32> = dep::answer\nret answer() + first() + second() + f() }",
                ),
            ),
            ("dep".into(), library.clone()),
            ("dep/api".into(), library),
            ("dep/utils".into(), module("fn helper() -> i32 { ret 2 }")),
        ]),
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
        HashMap::from([("dep".into(), "dep/api".into())]),
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(output.mir.functions.len(), 4);
    assert!(glyph_core::mir_verify::verify_module(&output.mir).is_empty());
}

#[test]
fn alias_chains_resolve_to_one_canonical_body() {
    let library = module("fn answer() -> i32 { ret 2 }");
    let output = compile_modules_with_aliases(
        HashMap::from([
            (
                "main".into(),
                module("from dep import answer\nfn main() -> i32 { ret answer() }"),
            ),
            ("dep".into(), library.clone()),
            ("dep/alias".into(), library.clone()),
            ("dep/api".into(), library),
        ]),
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
        HashMap::from([
            ("dep".into(), "dep/alias".into()),
            ("dep/alias".into(), "dep/api".into()),
        ]),
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(output.mir.functions.len(), 2);
}

#[test]
fn invalid_aliases_report_diagnostics_instead_of_dropping_definitions() {
    for aliases in [
        HashMap::from([("dep".into(), "missing".into())]),
        HashMap::from([("dep".into(), "dep".into())]),
        HashMap::from([
            ("dep".into(), "other".into()),
            ("other".into(), "dep".into()),
        ]),
        HashMap::from([("dep".into(), "other".into())]),
    ] {
        let output = compile_modules_with_aliases(
            HashMap::from([
                (
                    "main".into(),
                    module("from dep import answer\nfn main() -> i32 { ret answer() }"),
                ),
                ("dep".into(), module("fn answer() -> i32 { ret 2 }")),
                ("other".into(), module("fn answer() -> i32 { ret 3 }")),
            ]),
            "main",
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
            aliases,
        );
        assert!(!output.diagnostics.is_empty());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|d| d.message.contains("module alias"))
        );
    }
}

#[test]
fn colliding_local_closure_factory_retains_its_thread_send_certificate() {
    let output = glyph_frontend::compile_modules(
        HashMap::from([
            (
                "main".into(),
                module(
                    "from std/thread import spawn\nimport a\nfn make_task() -> FnOnce<(), ()> { let message: String = String::from_str(\"returned\")\nret move () -> { let length = message.len() } }\nfn main() -> i32 { let task: FnOnce<(), ()> = make_task()\nlet outcome = spawn(task)\nret 0 }",
                ),
            ),
            ("a".into(), module("fn make_task() -> i32 { ret 1 }")),
        ]),
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.insts)
            .any(|inst| {
                matches!(
                    inst,
                    glyph_core::mir::MirInst::Assign {
                        value: glyph_core::mir::Rvalue::ThreadSpawnUnit { .. },
                        ..
                    }
                )
            })
    );
}

#[test]
fn extern_main_cannot_silently_resolve_to_the_executable_entry() {
    for link_alias in 0..3 {
        let mut external = module("extern \"C\" fn main() -> i32;");
        let imported = if link_alias == 1 {
            let glyph_core::ast::Item::ExternFunction(function) = &mut external.items[0] else {
                unreachable!();
            };
            function.name.0 = "foreign_entry".into();
            function.link_name = Some("main".into());
            "foreign_entry"
        } else {
            if link_alias == 2 {
                let glyph_core::ast::Item::ExternFunction(function) = &mut external.items[0] else {
                    unreachable!();
                };
                function.link_name = Some("abs".into());
            }
            "main"
        };
        let output = glyph_frontend::compile_modules(
            HashMap::from([
                (
                    "main".into(),
                    module(&format!(
                        "from foreign import {imported} as c_main\nfn main() -> i32 {{ ret c_main() }}"
                    )),
                ),
                ("foreign".into(), external),
            ]),
            "main",
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );
        assert!(
            output
                .diagnostics
                .iter()
                .any(|d| d.message.contains("extern symbol 'main' conflicts"))
        );
    }
}
