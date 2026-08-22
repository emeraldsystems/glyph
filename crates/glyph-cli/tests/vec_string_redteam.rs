#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use glyph_frontend::{FrontendOptions, compile_source};

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

    if std::env::var("GLYPH_DEBUG_IR").is_ok() {
        let ir = ctx.dump_ir();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("vec_string_redteam_debug.ll"), ir).unwrap();
        std::fs::write(
            root.join("vec_string_redteam_mir_debug.txt"),
            format!("{:#?}\n", frontend_output.mir),
        )
        .unwrap();
        if std::env::var("GLYPH_SKIP_RUN").is_ok() {
            return 0;
        }
    }

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

// T6: Baseline — Vec<String> works for standalone variables
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_string_push_index_basic() {
    let source = r#"
        from std import String
        from std/string import byte_at

        fn main() -> i32 {
          let mut v: Vec<String> = Vec::new()
          v.push(String::from_str("hello"))
          v.push(String::from_str("world"))
          let s0: str = v[0]
          let s1: str = v[1]
          if s0.len() != 5 { ret 1 }
          if s1.len() != 5 { ret 2 }
          if byte_at(s0, 0) != 104 { ret 3 }
          if byte_at(s1, 0) != 119 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T7: Vec reallocation corrupts String elements after multiple growths.
// B4: codegen_vec_push() returns a snapshot via LLVMBuildLoad2, so subsequent
// pushes that trigger growth leave the local pointing at a stale buffer.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_string_growth_stress() {
    let source = r#"
        from std import String
        from std/string import byte_at

        fn main() -> i32 {
          let mut v: Vec<String> = Vec::new()
          let mut i: i32 = 0
          while i < 20 {
            v.push(String::from_str("abcdefghij"))
            i = i + 1
          }
          let s0: str = v[0]
          let s19: str = v[19]
          if s0.len() != 10 { ret 1 }
          if s19.len() != 10 { ret 2 }
          if byte_at(s0, 0) != 97 { ret 3 }
          if byte_at(s19, 9) != 106 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T8: B1 (leak, not crash) — struct String field is accessible but leaked on drop
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_with_string_field_access() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Named { label: String, value: i32 }

        fn main() -> i32 {
          let n = Named { label: String::from_str("test"), value: 42 }
          let lbl: str = n.label
          if lbl.len() != 4 { ret 1 }
          if byte_at(lbl, 0) != 116 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T9: B1 + B2 — struct with Vec<String> field, accessing simple field.
// Passes but all heap memory is leaked (B1).
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_with_vec_string_field_access() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Container { name: String, items: Vec<String> }

        fn main() -> i32 {
          let mut items: Vec<String> = Vec::new()
          items.push(String::from_str("alpha"))
          items.push(String::from_str("beta"))
          let c = Container { name: String::from_str("test"), items: items }
          let n: str = c.name
          if n.len() != 4 { ret 1 }
          if byte_at(n, 0) != 116 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T10: B2 — resolver correctly prevents moving Vec field out of struct.
// The shallow copy via `let v_copy = c.items` is rejected at the resolver level
// ("cannot move field 'items' out of a struct"), which is the correct behavior
// that prevents the aliased-pointer bug B2 describes.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_vec_field_read_elements() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Container { items: Vec<String> }

        fn main() -> i32 {
          let mut items: Vec<String> = Vec::new()
          items.push(String::from_str("alpha"))
          items.push(String::from_str("beta"))
          let c = Container { items: items }
          let v_copy = c.items
          let s0: str = v_copy[0]
          if s0.len() != 5 { ret 1 }
          if byte_at(s0, 0) != 97 { ret 2 }
          let s1: str = v_copy[1]
          if s1.len() != 4 { ret 3 }
          if byte_at(s1, 0) != 98 { ret 4 }
          ret 0
        }
    "#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("cannot move field 'items' out of a struct")),
        "expected the ownership diagnostic, got {:?}",
        output.diagnostics
    );
    assert!(
        output.mir.functions.is_empty(),
        "rejected field move must not reach executable MIR"
    );
}

// T11: B1 at scale — 100 iterations, each leaking a String allocation.
// Under sanitizer/valgrind this would show 100 leaked allocations.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_string_drop_loop() {
    let source = r#"
        from std import String

        struct Named { label: String, value: i32 }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let n = Named { label: String::from_str("iteration"), value: i }
            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T12: B1 — 50 iterations, each leaking a Vec buffer + String.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_vec_string_drop_loop() {
    let source = r#"
        from std import String

        struct Holder { data: Vec<String> }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let mut v: Vec<String> = Vec::new()
            v.push(String::from_str("entry"))
            let h = Holder { data: v }
            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T13: B1 + sret return — struct returned via sret, fields accessible, but never dropped
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_vec_field_returned_from_fn() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Container { name: String, items: Vec<String> }

        fn make_container() -> Container {
          let mut items: Vec<String> = Vec::new()
          items.push(String::from_str("alpha"))
          items.push(String::from_str("beta"))
          ret Container { name: String::from_str("made"), items: items }
        }

        fn main() -> i32 {
          let c = make_container()
          let n: str = c.name
          if n.len() != 4 { ret 1 }
          if byte_at(n, 0) != 109 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T14: B5 — struct passed by value creates shallow copy. Both caller and
// callee drop the struct, causing double-free of String/Vec fields.
// Needs deep-copy at call sites or parameter drop suppression.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_vec_field_passed_to_fn() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Container { name: String, items: Vec<String> }

        fn get_name(c: Container) -> i32 {
          let n: str = c.name
          if n.len() != 4 { ret 1 }
          if byte_at(n, 0) != 116 { ret 2 }
          ret 0
        }

        fn main() -> i32 {
          let mut items: Vec<String> = Vec::new()
          items.push(String::from_str("one"))
          let c = Container { name: String::from_str("test"), items: items }
          ret get_name(c)
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T15: B3 — Vec<Entry> elements' String fields not dropped on Vec drop.
// Also tests Vec indexing returning struct by value (shallow copy of Entry).
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_of_struct_with_string() {
    let source = r#"
        from std import String
        from std/string import byte_at

        struct Entry { key: String, value: i32 }

        fn main() -> i32 {
          let mut v: Vec<Entry> = Vec::new()
          v.push(Entry { key: String::from_str("alpha"), value: 1 })
          v.push(Entry { key: String::from_str("beta"), value: 2 })
          v.push(Entry { key: String::from_str("gamma"), value: 3 })
          let e0 = v[0]
          let k0: str = e0.key
          if k0.len() != 5 { ret 1 }
          if byte_at(k0, 0) != 97 { ret 2 }
          let e2 = v[2]
          let k2: str = e2.key
          if k2.len() != 5 { ret 3 }
          if byte_at(k2, 0) != 103 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// T16: Two independent Vec<String> with interleaved growth.
// Same B4 growth corruption as T7 — after multiple pushes, Vec elements are stale.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn multiple_vec_string_independent() {
    let source = r#"
        from std import String
        from std/string import byte_at

        fn main() -> i32 {
          let mut v1: Vec<String> = Vec::new()
          let mut v2: Vec<String> = Vec::new()
          let mut i: i32 = 0
          while i < 10 {
            v1.push(String::from_str("aaaa"))
            v2.push(String::from_str("bbbb"))
            i = i + 1
          }
          let s1: str = v1[0]
          let s2: str = v2[0]
          if byte_at(s1, 0) != 97 { ret 1 }
          if byte_at(s2, 0) != 98 { ret 2 }
          let len1: usize = v1.len()
          let len2: usize = v2.len()
          if len1 != 10 { ret 3 }
          if len2 != 10 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
