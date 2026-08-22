//! Non-owning views (Map.get / Vec[i] / field access results, marked
//! skip_drop in MIR) share heap data with their container. When such a view
//! escapes an ownership boundary — by-value call argument, return value,
//! container insert, struct/enum construction — codegen deep-clones it so
//! both the receiver and the container can drop independently.
//!
//! Regression for the double-free family: Map<String, JsonValue> get →
//! by-value call → Vec payload rebind crashed in libsystem_malloc
//! (mfm_free SIGTRAP) before deep-clone-on-escape existed.

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

// The minimal shape of the historical crash: JSON object → Map.get →
// by-value call → Array payload rebound to a mut local → dropped by callee,
// then the map dropped by the caller. Formerly SIGTRAP in mfm_free.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_get_vec_payload_through_function_boundary() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/vec import Vec

        from std/json/parser import parse

        fn check_data(data_value: JsonValue) -> i32 {
          ret match data_value {
            Array(values0) => {
              let mut values = values0
              let v3 = values.pop()
              match v3 {
                Some(_a) => 0,
                None => 4,
              }
            }
            _ => 9,
          }
        }

        fn main() -> i32 {
          let r = parse("{\"data\":[null,false,1.5e2,\"x\"]}")
          ret match r {
            Ok(v) => match v {
              Object(obj) => {
                let data_opt = obj.get(String::from_str("data"))
                match data_opt {
                  Some(data_value) => check_data(data_value),
                  None => 13,
                }
              }
              _ => 15,
            },
            Err(_e) => 16,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// A view returned by value must be cloned so the caller owns its copy.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_get_string_returned_by_value() {
    let source = r#"
        from std/enums import Option
        from std/map import Map
        from std/string import byte_at

        fn fetch(m: &Map<String, String>) -> String {
          let got = m.get(String::from_str("k"))
          ret match got {
            Some(v) => v,
            None => String::from_str("missing"),
          }
        }

        fn main() -> i32 {
          let mut m: Map<String, String> = Map::new()
          let _ = m.add(String::from_str("k"), String::from_str("abc"))
          let out = fetch(&m)
          let sr: str = out
          if sr.len() != 3 { ret 1 }
          if byte_at(sr, 0) != 97 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// A view pushed into a Vec transfers ownership to the Vec; both the Vec and
// the original map must drop cleanly.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_get_string_pushed_into_vec() {
    let source = r#"
        from std/enums import Option
        from std/map import Map
        from std/vec import Vec

        fn main() -> i32 {
          let mut m: Map<String, String> = Map::new()
          let _ = m.add(String::from_str("k1"), String::from_str("v1"))
          let _ = m.add(String::from_str("k2"), String::from_str("v2"))

          let mut collected: Vec<String> = Vec::new()
          let g1 = m.get(String::from_str("k1"))
          match g1 {
            Some(v1) => { collected.push(v1) },
            None => { ret 1 },
          }
          let g2 = m.get(String::from_str("k2"))
          match g2 {
            Some(v2) => { collected.push(v2) },
            None => { ret 2 },
          }

          if collected.len() != 2 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// A view stored as a map value must be cloned; dropping both maps must not
// double-free the shared string.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_get_string_inserted_into_other_map() {
    let source = r#"
        from std/enums import Option
        from std/map import Map

        fn main() -> i32 {
          let mut src: Map<String, String> = Map::new()
          let _ = src.add(String::from_str("k"), String::from_str("shared"))

          let mut dst: Map<String, String> = Map::new()
          let got = src.get(String::from_str("k"))
          match got {
            Some(v) => { let _ = dst.add(String::from_str("copy"), v) },
            None => { ret 1 },
          }

          if dst.has(String::from_str("copy")) == false { ret 2 }
          if src.has(String::from_str("k")) == false { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Moving a droppable field out of a struct is rejected by the resolver (the
// diagnostic suggests borrowing or cloning); the supported escape is an
// explicit clone, after which the struct must remain intact.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn field_access_string_passed_by_value() {
    let source = r#"
        from std/string import byte_at

        struct Person {
          name: String,
        }

        fn shout(name: String) -> i32 {
          let sr: str = name
          if sr.len() != 3 { ret 1 }
          ret 0
        }

        fn main() -> i32 {
          let p = Person { name: String::from_str("bob") }
          let code = shout(p.name.clone())
          if code != 0 { ret code }
          let sr: str = p.name
          if byte_at(sr, 0) != 98 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Heap-hygiene loop: repeat the historical crash shape many times so any
// residual double-free or corruption trips the allocator.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_get_escape_loop_no_heap_corruption() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/vec import Vec

        from std/json/parser import parse

        fn count_items(data_value: JsonValue) -> i32 {
          ret match data_value {
            Array(values0) => {
              let mut values = values0
              let mut n: i32 = 0
              let mut keep_going: i32 = 1
              while keep_going > 0 {
                let item = values.pop()
                match item {
                  Some(_v) => { n = n + 1 },
                  None => { keep_going = 0 },
                }
              }
              n
            }
            _ => 0 - 1,
          }
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let r = parse("{\"data\":[1,2,3,\"four\",[5]]}")
            match r {
              Ok(v) => match v {
                Object(obj) => {
                  let data_opt = obj.get(String::from_str("data"))
                  match data_opt {
                    Some(data_value) => {
                      let n = count_items(data_value)
                      if n != 5 { ret 20 }
                    }
                    None => { ret 21 },
                  }
                }
                _ => { ret 22 },
              },
              Err(_e) => { ret 23 },
            }
            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
