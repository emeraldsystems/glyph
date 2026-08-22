use glyph_core::ast::Item;
use glyph_frontend::{FrontendOptions, compile_source, std_modules};

fn compile(source: &str, include_std: bool) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std,
        },
    )
}

fn messages(source: &str) -> Vec<String> {
    compile(source, true)
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn canonical_thread_module_declares_scope_and_scoped_handle_methods() {
    let modules = std_modules();
    let thread = modules.get("std/thread").expect("std/thread module");

    let scope_function = thread
        .items
        .iter()
        .any(|item| matches!(item, Item::Function(function) if function.name.0 == "scope"));
    let scope_spawn = thread.items.iter().any(|item| {
        matches!(item, Item::Struct(definition)
            if definition.name.0 == "Scope"
                && definition.methods.iter().any(|method| method.name.0 == "spawn"))
    });
    let scoped_join = thread.items.iter().any(|item| {
        matches!(item, Item::Struct(definition)
            if definition.name.0 == "ScopedJoinHandle"
                && definition.methods.iter().any(|method| method.name.0 == "join"))
    });

    assert!(scope_function, "missing std::thread::scope declaration");
    assert!(scope_spawn, "missing Scope::spawn declaration");
    assert!(scoped_join, "missing ScopedJoinHandle::join declaration");
}

#[test]
fn importing_scope_populates_its_canonical_dependency_types() {
    let output = compile(
        r#"
import scope from std/thread

fn main() -> i32 { ret 0 }
"#,
        true,
    );

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn canonical_scoped_values_are_rejected_in_escaping_type_placements() {
    let field = messages(
        r#"
import Scope from std/thread

struct Holder { scope: Scope }
fn main() -> i32 { ret 0 }
"#,
    );
    assert!(
        field
            .iter()
            .any(|message| message.contains("scoped-thread token")
                && message.contains("struct field")),
        "{field:?}"
    );

    let returned = messages(
        r#"
import ScopedJoinHandle from std/thread

fn invalid(handle: ScopedJoinHandle<i32>) -> ScopedJoinHandle<i32> { ret handle }
fn main() -> i32 { ret 0 }
"#,
    );
    assert!(
        returned
            .iter()
            .any(|message| message.contains("scoped-thread token") && message.contains("return")),
        "{returned:?}"
    );
}

#[test]
fn same_named_user_types_do_not_gain_canonical_scope_restrictions() {
    let output = compile(
        r#"
struct Scope {}
struct Holder { scope: Scope }

fn identity(scope: Scope) -> Scope { ret scope }
fn main() -> i32 { ret 0 }
"#,
        false,
    );

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn canonical_scoped_names_cannot_be_forged_without_std_identity() {
    let output = compile(
        r#"
fn forged(scope: std::thread::Scope) -> i32 { ret 0 }
fn main() -> i32 { ret 0 }
"#,
        false,
    );
    let messages: Vec<_> = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();

    assert!(
        messages
            .iter()
            .any(|message| message.contains("compiler-owned identities")
                && message.contains("std/thread")),
        "{messages:?}"
    );
}
