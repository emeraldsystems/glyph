//! GLYPH-4: enum-sensitive lowering (construction, `match`, and `?`) now
//! resolves a variant's declared index and payload type through a single
//! shared helper (`mir_lower::enums::{find_variant, resolve_enum_variant}`)
//! instead of several independent `enum_def.variants.iter().position(...)`
//! searches. These tests exercise the behavior that helper must preserve:
//! stdlib `Option`/`Result` variant order, a custom enum declared in
//! non-alphabetical order, a custom enum whose variant names collide with
//! stdlib `Option`/`Result` variant names but in the OPPOSITE declared
//! order (index resolution must not bleed across enums), and payload-type
//! resolution for a multi-field (tuple) payload.

#[cfg(all(feature = "codegen", unix))]
use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use std::os::unix::process::ExitStatusExt;

#[cfg(all(feature = "codegen", unix))]
use std::process::Command;

#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return 0;
    }

    let frontend_output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        frontend_output.diagnostics.is_empty(),
        "Compilation failed with diagnostics: {:?}",
        frontend_output.diagnostics
    );

    let temp = TempDir::new().unwrap();
    let obj_path = temp.path().join("test.o");
    let exe_path = temp.path().join("test_exe");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return 0;
    }
    ctx.emit_object_file(&obj_path).unwrap();

    let linker = Linker::new();
    let opts = LinkerOptions {
        output_path: exe_path.clone(),
        object_files: vec![obj_path],
        link_libs: Vec::new(),
        link_search_paths: Vec::new(),
        runtime_lib_path: Linker::get_runtime_lib_path(),
    };
    linker.link(&opts).unwrap();

    let status = Command::new(&exe_path).status().unwrap();
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        -sig
    } else {
        -1
    }
}

/// Baseline: stdlib `Option`'s declared order (`None` = 0, `Some` = 1, per
/// `crates/glyph-frontend/src/stdlib.rs`) must still round-trip through
/// construction and `match` after centralizing variant lookup.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn stdlib_option_variant_order_round_trips() {
    let source = r#"
        from std/enums import Option

        fn describe(o: Option<i32>) -> i32 {
          ret match o {
            Some(x) => x,
            None => -1,
          }
        }

        fn main() -> i32 {
          let a: Option<i32> = Some(7)
          let b: Option<i32> = None
          if describe(a) != 7 { ret 1 }
          if describe(b) != -1 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// Baseline: stdlib `Result`'s declared order (`Ok` = 0, `Err` = 1) must
/// still round-trip through construction, `match`, and the `?` operator.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn stdlib_result_variant_order_round_trips() {
    let source = r#"
        from std/enums import Result

        fn describe(r: Result<i32, i32>) -> i32 {
          ret match r {
            Ok(x) => x,
            Err(e) => 0 - e,
          }
        }

        fn passthrough(r: Result<i32, i32>) -> Result<i32, i32> {
          let v = r?
          ret Ok(v)
        }

        fn main() -> i32 {
          let a: Result<i32, i32> = Ok(9)
          let b: Result<i32, i32> = Err(4)
          if describe(a) != 9 { ret 1 }
          if describe(b) != -4 { ret 2 }
          let c: Result<i32, i32> = Ok(9)
          if describe(passthrough(c)) != 9 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// A custom enum declared in non-alphabetical order must resolve every
/// variant's index by its declaration position, not by any name-based
/// ordering assumption.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn custom_enum_non_alphabetical_order_resolves_by_declaration() {
    let source = r#"
        enum Season {
          Winter,
          Autumn,
          Summer,
          Spring,
        }

        fn code(s: Season) -> i32 {
          ret match s {
            Winter => 0,
            Autumn => 1,
            Summer => 2,
            Spring => 3,
          }
        }

        fn main() -> i32 {
          if code(Season::Winter) != 0 { ret 1 }
          if code(Season::Autumn) != 1 { ret 2 }
          if code(Season::Summer) != 2 { ret 3 }
          if code(Season::Spring) != 3 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// A custom enum whose variants are named `Some`/`None`, declared in the
/// OPPOSITE order from stdlib `Option` (`Some` = 0, `None` = 1 here, vs.
/// `None` = 0, `Some` = 1 in stdlib), must resolve against its OWN
/// declaration order even when stdlib `Option` is also imported and in
/// scope. A shared-by-name lookup that accidentally picked the stdlib
/// index for the wrong enum would misclassify `MyOpt::None` as index 1
/// matching stdlib's `Some` branch, corrupting the result.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn custom_enum_reversed_names_does_not_pick_stdlib_index() {
    let source = r#"
        from std/enums import Option

        enum MyOpt {
          Some(i32),
          None,
        }

        fn classify(v: MyOpt) -> i32 {
          ret match v {
            Some(x) => x,
            None => -1,
          }
        }

        fn main() -> i32 {
          let a = MyOpt::Some(42)
          let b = MyOpt::None
          // Constructed via the QUALIFIED ctor name to avoid a separate,
          // pre-existing ambiguity in bare (unqualified) enum-variant
          // constructor name resolution when two in-scope enums declare a
          // variant with the same name (tracked as a follow-up, not part of
          // GLYPH-4's variant index/payload lookup helper). The `match`
          // below is what actually exercises the shared lookup helper: it
          // resolves the bare `Some`/`None` patterns against `stdlib_opt`'s
          // known scrutinee type (`Option<i32>`), which must not bleed into
          // `MyOpt`'s reversed declaration order.
          let stdlib_opt: Option<i32> = Option::Some(5)
          if classify(a) != 42 { ret 1 }
          if classify(b) != -1 { ret 2 }
          ret match stdlib_opt {
            Some(x) => x - 5,
            None => 3,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// A custom enum whose variants are named `Ok`/`Err`, in the opposite order
/// from stdlib `Result`, must likewise resolve against its own declaration.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn custom_enum_reversed_ok_err_does_not_pick_stdlib_index() {
    let source = r#"
        from std/enums import Result

        enum MyResult {
          Err(i32),
          Ok(i32),
        }

        fn classify(v: MyResult) -> i32 {
          ret match v {
            Ok(x) => x,
            Err(e) => 0 - e,
          }
        }

        fn main() -> i32 {
          let a = MyResult::Ok(11)
          let b = MyResult::Err(6)
          if classify(a) != 11 { ret 1 }
          if classify(b) != -6 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// Payload-type resolution through the helper must handle a variant with a
/// multi-field (tuple) payload, not just a single scalar payload.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn multi_field_payload_variant_resolves_payload_type() {
    let source = r#"
        enum Shape {
          Point,
          Rect((i32, i32)),
        }

        fn area(s: Shape) -> i32 {
          ret match s {
            Point => 0,
            Rect(dims) => dims.0 * dims.1,
          }
        }

        fn main() -> i32 {
          if area(Shape::Point) != 0 { ret 1 }
          if area(Shape::Rect((3, 4))) != 12 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
