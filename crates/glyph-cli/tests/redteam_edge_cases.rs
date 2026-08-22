/// Red-team edge-case tests for ownership, drops, and memory safety.
///
/// These tests target historical ownership, drop, and memory-safety gaps:
/// - Enum payload drops
/// - Map entry drops
/// - Break/continue scope drops
/// - Droppable pass-by-value ownership
/// - Vec growth and element ownership
///
/// Historical crash probes have been converted to deterministic regressions;
/// this file contains no ignored safety tests.

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

// ---------------------------------------------------------------------------
// T1: Enum with String payload constructed 100x in loop — leaked silently.
// BUG: codegen_drop_named_slot returns Ok(()) for enums.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn enum_string_payload_loop_leak() {
    let source = r#"
        enum StringOrNone {
          HasStr(String)
          Empty
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let val = HasStr(String::from_str("leaked"))
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 100 String allocations (enum drop is a no-op).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T2: Enum payload extraction via match — verify data readable.
// BUG: Enum drop no-op means the extracted String leaks after match.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn enum_string_match_extract() {
    let source = r#"
        from std/string import byte_at

        enum MaybeString {
          Just(String)
          Nothing
        }

        fn main() -> i32 {
          let val = Just(String::from_str("hello"))
          let result = match val {
            Just(s) => {
              let sr: str = s
              if sr.len() != 5 { ret 1 }
              if byte_at(sr, 0) != 104 { ret 2 }
              0
            },
            Nothing => 3,
          }
          ret result
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T3: 100 iterations of enum creation + match extraction.
// BUG: Enum drop no-op — each iteration leaks.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn enum_string_match_loop() {
    let source = r#"
        from std/string import byte_at

        enum MaybeStr {
          Has(String)
          Empty
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let val = Has(String::from_str("cycle"))
            let ok = match val {
              Has(s) => {
                let sr: str = s
                if sr.len() != 5 { ret 1 }
                0
              },
              Empty => 2,
            }
            if ok != 0 { ret ok }
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 100 String allocations.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T4: Map<String, String> created 50x in loop — all memory leaked.
// BUG: codegen_drop_map returns Ok(()) unconditionally.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_string_string_loop_leak() {
    let source = r#"
        from std/map import Map

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let m: Map<String, String> = Map::new()
            let _add = m.add(String::from_str("key"), String::from_str("val"))
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 50 Map + String allocations (map drop is a no-op).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T5: String local created before `break` — not dropped.
// BUG: Break emits Goto without dropping scope locals.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn break_with_string_local_no_drop() {
    let source = r#"
        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let s = String::from_str("break_me")
            if i == 25 { break }
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 26 String allocations (iterations 0..=25).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T6: String local created before `continue` — not dropped.
// BUG: Continue emits Goto without dropping scope locals.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn continue_with_string_local_no_drop() {
    let source = r#"
        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            i = i + 1
            let s = String::from_str("skip_me")
            if i > 25 { continue }
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 24 String allocations (iterations where i > 25).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T7: Inner match consumes String, outer match has different arm.
// BUG: "Moved wins" merge in match state may prevent drop.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_match_string_payload_leak() {
    let source = r#"
        enum Outer {
          A(String)
          B
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let val = A(String::from_str("nested"))
            let code = match val {
              A(s) => {
                let sr: str = s
                if sr.len() != 6 { ret 1 }
                0
              },
              B => 0,
            }
            if code != 0 { ret code }
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks — enum drop is no-op, match arm extraction
    // may not transfer ownership correctly.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T8: Early `ret` from nested scope — drop_all_active_locals should handle.
// Should work correctly since Ret calls drop_all_active_locals.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn return_from_nested_scope_drops_outer() {
    let source = r#"
        struct Wrapper { name: String }

        fn inner() -> i32 {
          let w = Wrapper { name: String::from_str("outer") }
          if true {
            let s = String::from_str("inner")
            ret 0
          }
          ret 1
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let code = inner()
            if code != 0 { ret code }
            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T9: Struct reassignment in loop — handle_reassign emits drop before
// reassigning struct with String field, 50 iterations.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_reassign_in_loop_drops_old() {
    let source = r#"
        struct Named { label: String, value: i32 }

        fn main() -> i32 {
          let mut n = Named { label: String::from_str("init"), value: 0 }
          let mut i: i32 = 1
          while i < 50 {
            n = Named { label: String::from_str("updated"), value: i }
            i = i + 1
          }
          let lbl: str = n.label
          if lbl.len() != 7 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T10: Vec<Entry> where Entry has two String fields — recursive element drop.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_struct_droppable_fields_recursive_drop() {
    let source = r#"
        struct Entry { key: String, value: String }

        fn main() -> i32 {
          let mut v: Vec<Entry> = Vec::new()
          v.push(Entry { key: String::from_str("k1"), value: String::from_str("v1") })
          v.push(Entry { key: String::from_str("k2"), value: String::from_str("v2") })
          v.push(Entry { key: String::from_str("k3"), value: String::from_str("v3") })
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T11: Struct with two String fields passed by value.
// Regression: by-value calls consume the caller's owner so caller/callee do not
// both drop aliased fields.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_pass_by_value_double_free() {
    let source = r#"
        struct TwoStrings { a: String, b: String }

        fn consume(ts: TwoStrings) -> i32 {
          let sa: str = ts.a
          if sa.len() != 5 { ret 1 }
          ret 0
        }

        fn main() -> i32 {
          let ts = TwoStrings { a: String::from_str("hello"), b: String::from_str("world") }
          ret consume(ts)
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T12: s = s.concat(...) in loop, 100 iterations.
// Tests reassign + consume interaction on Strings.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn string_concat_reassign_loop() {
    let source = r#"
        fn main() -> i32 {
          let mut s = String::from_str("a")
          let mut i: i32 = 0
          while i < 100 {
            s = s.concat(String::from_str("b"))
            i = i + 1
          }
          let sr: str = s
          if sr.len() != 101 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T13: Variable initialized only in then-branch — uninitialized path
// should not drop.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn conditional_init_no_false_drop() {
    let source = r#"
        fn main() -> i32 {
          let cond: bool = true
          if cond {
            let s = String::from_str("only_here")
            let sr: str = s
            if sr.len() != 9 { ret 1 }
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T14: Map with i32 keys — add 20 entries, verify has() works.
// BUG: Map drop is a no-op so all entries leak.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_string_keys_loop_stress() {
    let source = r#"
        from std/map import Map

        fn main() -> i32 {
          let m: Map<i32, i32> = Map::new()
          let mut i: i32 = 0
          while i < 20 {
            let _add = m.add(i, i + 100)
            i = i + 1
          }
          let has10 = m.has(10)
          if !has10 { ret 1 }
          let has0 = m.has(0)
          if !has0 { ret 2 }
          let has99 = m.has(99)
          if has99 { ret 3 }
          ret 0
        }
    "#;

    // Map drop is a no-op — all 20 entries leak.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T15: Access element after 30+ pushes triggers Vec growth.
// BUG: B4 — push returns stale snapshot after reallocation.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn vec_push_result_after_growth() {
    let source = r#"
        fn main() -> i32 {
          let mut v: Vec<i32> = Vec::new()
          let mut i: i32 = 0
          while i < 30 {
            v.push(i)
            i = i + 1
          }
          let first = v[0]
          if first != 0 { ret 1 }
          let last = v[29]
          if last != 29 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T16: Break from inside `if` inside loop with String locals at both
// scope levels.
// BUG: Break emits Goto without dropping locals at any scope level.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn break_from_nested_if_inside_loop() {
    let source = r#"
        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let outer_s = String::from_str("outer")
            if i == 25 {
              let inner_s = String::from_str("inner")
              break
            }
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks Strings at both scope levels on break path.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T17: Option<String> from std — most production-realistic. Loop 100x.
// BUG: Enum drop no-op applies to monomorphized Option<String>.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn option_string_payload_no_drop() {
    let source = r#"
        from std/enums import Option

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let val: Option<String> = Some(String::from_str("option_leak"))
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 100 String allocations (Option drop is no-op).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T18: Result<i32, Err> with Err payload (Err has String msg field).
// Loop 50x.
// BUG: Enum drop no-op — Result<i32, Err> is an enum.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn result_string_error_drop() {
    let source = r#"
        from std/enums import Result
        from std/enums import Err

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let val: Result<i32, Err> = Err(Err { msg: String::from_str("error_leak") })
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 50 Err struct + String allocations.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T19: Own<i32> inside enum variant, loop 100x — heap leak.
// BUG: Enum drop no-op means Own pointer is never freed.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn own_ptr_in_enum_payload_leak() {
    let source = r#"
        enum OwnOrNot {
          Has(Own<i32>)
          Not
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let val = Has(Own::new(42))
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks 100 heap allocations (enum drop is a no-op).
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T20: Struct containing enum field — struct drop iterates fields but
// enum field drop returns Ok(()).
// BUG: Struct drop recursion hits enum no-op for the enum field.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_with_enum_field_drop() {
    let source = r#"
        enum Payload {
          Text(String)
          None
        }

        struct Container {
          id: i32
          data: Payload
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 50 {
            let c = Container { id: i, data: Text(String::from_str("field_leak")) }
            i = i + 1
          }
          ret 0
        }
    "#;

    // Exits 0 but leaks — struct drop recurses into fields, but enum
    // field drop is a no-op, so the String payload leaks.
    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T21: if/else branch-local struct reassignment stress.
// Targets pre-assignment drop + join behavior under repeated branch flips.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn if_else_struct_reassign_branch_stress() {
    let source = r#"
        struct Pair { a: String, b: String }

        fn main() -> i32 {
          let mut pair = Pair { a: String::from_str("seed"), b: String::from_str("seed") }
          let mut i: i32 = 0
          while i < 200 {
            if (i % 2) == 0 {
              pair = Pair { a: String::from_str("left"), b: String::from_str("arm") }
            } else {
              pair = Pair { a: String::from_str("right"), b: String::from_str("arm") }
            }
            i = i + 1
          }
          let a_ref: str = pair.a
          if a_ref.len() < 4 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T22: match-arm struct reassignment stress.
// Targets per-arm reassignment/drop behavior when all arms join.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn match_struct_reassign_branch_stress() {
    let source = r#"
        enum Side { Left, Right }
        struct Pair { a: String, b: String }

        fn main() -> i32 {
          let mut pair = Pair { a: String::from_str("seed"), b: String::from_str("seed") }
          let mut i: i32 = 0
          while i < 200 {
            let side = if (i % 2) == 0 { Left() } else { Right() }
            let _code = match side {
              Left => {
                pair = Pair { a: String::from_str("left"), b: String::from_str("arm") }
                0
              },
              Right => {
                pair = Pair { a: String::from_str("right"), b: String::from_str("arm") }
                1
              },
            }
            i = i + 1
          }
          let b_ref: str = pair.b
          if b_ref.len() != 3 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T23: nested if-inside-match reassignment stress.
// Targets nested control-flow join interactions for drop/reassign logic.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn nested_if_inside_match_reassign_stress() {
    let source = r#"
        enum Side { Left, Right }
        struct Pair { a: String, b: String }

        fn main() -> i32 {
          let mut pair = Pair { a: String::from_str("s0"), b: String::from_str("s0") }
          let mut i: i32 = 0
          while i < 150 {
            let side = if (i % 2) == 0 { Left() } else { Right() }
            let _code = match side {
              Left => {
                if (i % 3) == 0 {
                  pair = Pair { a: String::from_str("l0"), b: String::from_str("aa") }
                } else {
                  pair = Pair { a: String::from_str("l1"), b: String::from_str("bb") }
                }
                0
              },
              Right => {
                if (i % 5) == 0 {
                  pair = Pair { a: String::from_str("r0"), b: String::from_str("cc") }
                } else {
                  pair = Pair { a: String::from_str("r1"), b: String::from_str("dd") }
                }
                1
              },
            }
            i = i + 1
          }
          let a_ref: str = pair.a
          if a_ref.len() != 2 { ret 1 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T24: If-split pass-by-value regression.
// By-value calls inside branches consume the caller's owner.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_pass_by_value_if_branch_regression() {
    let source = r#"
        struct TwoStrings { a: String, b: String }

        fn consume(ts: TwoStrings) -> i32 {
          let sa: str = ts.a
          if sa.len() != 5 { ret 1 }
          ret 0
        }

        fn main() -> i32 {
          let ts = TwoStrings { a: String::from_str("hello"), b: String::from_str("world") }
          let cond: bool = true
          if cond {
            ret consume(ts)
          } else {
            ret 0
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

// ---------------------------------------------------------------------------
// T25: Match-split pass-by-value regression.
// By-value calls inside match arms consume the caller's owner.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn struct_pass_by_value_match_arm_regression() {
    let source = r#"
        enum Branch { Run, Skip }
        struct TwoStrings { a: String, b: String }

        fn consume(ts: TwoStrings) -> i32 {
          let sa: str = ts.a
          if sa.len() != 5 { ret 1 }
          ret 0
        }

        fn main() -> i32 {
          let ts = TwoStrings { a: String::from_str("hello"), b: String::from_str("world") }
          let branch = Run()
          let code = match branch {
            Run => consume(ts),
            Skip => 0,
          }
          ret code
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
