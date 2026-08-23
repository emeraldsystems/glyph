//! GLYPH-64: indexed assignment (`vec[i] = v`, `arr[i] = v`) including
//! through &mut Vec<T> parameters, read-modify-write loops, droppable
//! element overwrite (old value freed exactly once), and f32 element
//! stores. Also covers GLYPH-65: void functions whose last statement
//! produces a value (e.g. a trailing `.push()`) must discard it instead
//! of emitting a non-void return.

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

#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_and_array_element_assignment() {
    let source = r#"
        from std/vec import Vec

        fn bump(xs: &mut Vec<f64>, i: usize, delta: f64) -> i32 {
          xs[i] = xs[i] + delta
          ret 0
        }

        fn main() -> i32 {
          let mut xs: Vec<f64> = Vec::new()
          xs.push(1.0)
          xs.push(2.0)
          xs[1] = 5.0
          if xs[1] != 5.0 { ret 1 }
          let _b = bump(&mut xs, 0, 0.5)
          if xs[0] != 1.5 { ret 2 }

          let mut arr: [i32; 4] = [10, 20, 30, 40]
          arr[2] = 99
          if arr[2] != 99 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn read_modify_write_loop_and_f32_store() {
    let source = r#"
        from std/vec import Vec

        fn main() -> i32 {
          let mut vf: Vec<f64> = Vec::new()
          let mut i: i32 = 0
          while i < 8 {
            vf.push(0.0)
            i = i + 1
          }
          i = 0
          while i < 100 {
            let slot: usize = (i % 8) as usize
            vf[slot] = vf[slot] + 0.5
            i = i + 1
          }
          if vf[0] != 6.5 { ret 1 }
          if vf[7] != 6.0 { ret 2 }

          let mut vs: Vec<f32> = Vec::new()
          vs.push(0.0)
          vs[0] = 0.25
          if vs[0] != 0.25 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// Overwriting a droppable element must free the old value exactly once:
// no leak, no double-free at scope exit.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn droppable_element_overwrite_frees_once() {
    let source = r#"
        from std/vec import Vec
        from std/string import byte_at

        fn main() -> i32 {
          let mut names: Vec<String> = Vec::new()
          names.push(String::from_str("alpha"))
          names.push(String::from_str("beta"))
          let mut round: i32 = 0
          while round < 100 {
            names[0] = String::from_str("gamma")
            names[1] = String::from_str("delta")
            round = round + 1
          }
          let s: str = names[0]
          if s.len() != 5 { ret 1 }
          if byte_at(s, 0) != 103 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[test]
fn indexed_assignment_on_non_container_is_rejected() {
    let source = r#"import std

fn main() -> i32 {
  let mut x: i32 = 3;
  x[0] = 1
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
        output
            .diagnostics
            .iter()
            .any(|d| d.message.contains("must be a Vec or fixed-size array")),
        "expected container diagnostic, got: {:?}",
        output.diagnostics
    );
}

// GLYPH-65 regression: a void function ending in a value-producing builtin
// used to emit `ret <aggregate>` inside a void LLVM function.
#[cfg(all(feature = "codegen", unix))]
#[test]
fn void_fn_trailing_builtin_value_is_discarded() {
    let source = r#"
        from std/vec import Vec

        fn push_two(xs: &mut Vec<i32>) {
          xs.push(7)
          xs.push(9)
        }

        fn note_len(s: String) {
          let _n = s.len()
        }

        fn main() -> i32 {
          let mut xs: Vec<i32> = Vec::new()
          push_two(&mut xs)
          if xs.len() != 2 { ret 1 }
          if xs[0] + xs[1] != 16 { ret 2 }
          note_len(String::from_str("owned"))
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
