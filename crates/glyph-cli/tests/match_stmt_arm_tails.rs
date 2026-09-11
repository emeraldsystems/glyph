//! GLYPH-88: a `match` used as a statement lowers its arms as statement
//! blocks, so an arm whose block ends in an `if` without `else` or a `while`
//! is fine - its value is never used.
//!
//! Before the fix every `match` was lowered as a value expression. A trailing
//! `while` in an arm then failed with "while expression produces unit", and a
//! trailing `if` without `else` failed with "if expression is missing an else
//! branch; expected a value of type 'Vec<T>'" - the "expected" type being
//! whatever an *earlier* arm's last expression happened to produce (a `Vec`
//! snapshot from a trailing `.push()`), since the match's result local had
//! been typed from that arm. Both regressed with GLYPH-84 and appeared in
//! GlyphAudio's engine loop.
//!
//! A `match` whose value is actually used (`let x = match ..`, `ret match ..`,
//! a function's non-void tail) still requires a value from every arm.

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

fn diagnostics_for(source: &str) -> Vec<String> {
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    output
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

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
        panic!("test binary was killed by signal {sig}");
    } else {
        -1
    }
}

// ---------------------------------------------------------------------------
// Statement-position matches accept arms ending in `while` / bare `if`.
// ---------------------------------------------------------------------------

const ARM_ENDING_IN_WHILE: &str = r#"
    enum K { A, B }

    fn count(n: i32) -> i32 {
      let k = K::A
      let mut z = 0
      let mut total = 0
      match k {
        B => {},
        A => {
          while z < n {
            total = total + 1
            z = z + 1
          }
        },
      }
      ret total
    }

    fn main() -> i32 {
      if count(4) != 4 { ret 1 }
      ret 0
    }
"#;

#[test]
fn arm_ending_in_while_is_accepted() {
    assert_eq!(diagnostics_for(ARM_ENDING_IN_WHILE), Vec::<String>::new());
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn arm_ending_in_while_runtime() {
    assert_eq!(build_and_run_exit_code(ARM_ENDING_IN_WHILE), 0);
}

// The earlier arm's block ends in `acc.push(7)`, whose value is a `Vec<i32>`
// snapshot; that used to become the match's result type and then the
// "expected" type demanded from the later arm's trailing `if`.
const ARM_ENDING_IN_BARE_IF_AFTER_VEC_TAIL: &str = r#"
    enum K { A(i32), B }

    fn run(k: K) -> i32 {
      let mut acc: Vec<i32> = Vec::new()
      let mut flag = false
      match k {
        B => { acc.push(7) },
        A(n) => {
          acc.push(n)
          if n > 1 { flag = true }
        },
      }
      if flag { ret 1 }
      ret acc.len() as i32
    }

    fn main() -> i32 {
      if run(K::A(1)) != 1 { ret 2 }
      if run(K::A(5)) != 1 { ret 3 }
      if run(K::B) != 1 { ret 4 }
      ret 0
    }
"#;

#[test]
fn arm_ending_in_bare_if_after_vec_tail_is_accepted() {
    assert_eq!(
        diagnostics_for(ARM_ENDING_IN_BARE_IF_AFTER_VEC_TAIL),
        Vec::<String>::new()
    );
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn arm_ending_in_bare_if_after_vec_tail_runtime() {
    assert_eq!(build_and_run_exit_code(ARM_ENDING_IN_BARE_IF_AFTER_VEC_TAIL), 0);
}

// The ticket's shape: a statement `match` inside a `while` whose arm is
// itself a `match` whose arm block ends in a `while`.
const NESTED_MATCH_IN_LOOP: &str = r#"
    struct Header { frames: i32 }
    enum Cmd { Ping, Begin(Header), Quit }
    enum Slot { Value(Cmd), Empty }

    fn next(i: i32) -> Slot {
      if i == 0 { ret Slot::Value(Cmd::Ping) }
      if i == 1 { ret Slot::Value(Cmd::Begin(Header { frames: 10 })) }
      if i == 2 { ret Slot::Empty }
      ret Slot::Value(Cmd::Quit)
    }

    fn consume() -> i32 {
      let mut pool: Vec<f64> = Vec::new()
      let mut running = true
      let mut i = 0
      while running {
        let got = next(i)
        i = i + 1
        match got {
          Value(cmd) => match cmd {
            Ping => {},
            Begin(h) => {
              let mut z = 0
              while z < h.frames {
                pool.push(0.0)
                z = z + 1
              }
            },
            Quit => { running = false },
          },
          Empty => {},
        }
      }
      ret pool.len() as i32
    }

    fn main() -> i32 {
      if consume() != 10 { ret 1 }
      ret 0
    }
"#;

#[test]
fn nested_match_arm_ending_in_while_inside_loop_is_accepted() {
    assert_eq!(diagnostics_for(NESTED_MATCH_IN_LOOP), Vec::<String>::new());
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_match_arm_ending_in_while_inside_loop_runtime() {
    assert_eq!(build_and_run_exit_code(NESTED_MATCH_IN_LOOP), 0);
}

// A void function whose *tail* is such a match: the expected value is unit,
// so the arms are statements there as well.
const MATCH_AS_VOID_FUNCTION_TAIL: &str = r#"
    enum K { A(i32), B }

    fn fill(k: K, out: &mut Vec<i32>) {
      match k {
        B => { out.push(0) },
        A(n) => {
          let mut z = 0
          while z < n {
            out.push(z)
            z = z + 1
          }
        },
      }
    }

    fn main() -> i32 {
      let mut v: Vec<i32> = Vec::new()
      fill(K::A(3), &mut v)
      fill(K::B, &mut v)
      if v.len() != 4 { ret 1 }
      ret 0
    }
"#;

#[test]
fn match_as_void_function_tail_is_accepted() {
    assert_eq!(diagnostics_for(MATCH_AS_VOID_FUNCTION_TAIL), Vec::<String>::new());
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn match_as_void_function_tail_runtime() {
    assert_eq!(build_and_run_exit_code(MATCH_AS_VOID_FUNCTION_TAIL), 0);
}

// ---------------------------------------------------------------------------
// Value-position matches are unchanged.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "codegen", unix))]
#[test]
fn value_matches_still_produce_values_runtime() {
    let source = r#"
        enum K { A(i32), B }

        fn pick(k: K) -> i32 {
          match k {
            A(n) => n * 2,
            B => 7,
          }
        }

        fn main() -> i32 {
          let a = match K::A(4) { A(n) => n, B => 0, }
          if a != 4 { ret 1 }
          if pick(K::A(5)) != 10 { ret 2 }
          if pick(K::B) != 7 { ret 3 }
          ret match K::B { A(_n) => 9, B => 0, }
        }
    "#;
    assert_eq!(build_and_run_exit_code(source), 0);
}

#[test]
fn value_match_arm_ending_in_while_is_still_rejected() {
    let source = r#"
        enum K { A, B }

        fn main() -> i32 {
          let k = K::A
          let x: i32 = match k {
            A => {
              let mut z = 0
              while z < 3 { z = z + 1 }
            },
            B => 1,
          }
          ret x
        }
    "#;
    let diags = diagnostics_for(source);
    assert!(
        diags.iter().any(|d| d.contains("while expression produces unit")),
        "expected the value-position match to be rejected, got {diags:?}"
    );
}
