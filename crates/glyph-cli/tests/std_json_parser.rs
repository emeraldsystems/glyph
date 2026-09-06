use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use std::process::Command;

#[cfg(all(feature = "codegen", unix))]
use std::os::unix::process::ExitStatusExt;

#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return 0;
    }

    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_DEBUG_IR").is_ok() {
        println!("[build_and_run] compile start");
    }

    let frontend_output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_DEBUG_IR").is_ok() {
        println!("[build_and_run] compile done");
    }

    assert!(
        frontend_output.diagnostics.is_empty(),
        "Compilation failed with diagnostics: {:?}",
        frontend_output.diagnostics
    );

    if std::env::var("GLYPH_DEBUG_IR").is_ok() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).unwrap();
        let mir_path = root.join("std_json_parser_mir_debug.txt");
        std::fs::write(&mir_path, format!("{:#?}\n", frontend_output.mir)).unwrap();
        println!("[build_and_run] wrote MIR to {:?}", mir_path);
    }

    // Optional shortcut for debugging codegen without running the produced binary.
    let skip_run = std::env::var("GLYPH_SKIP_RUN").is_ok();

    let temp = TempDir::new().unwrap();
    let obj_path = temp.path().join("test.o");
    let exe_path = temp.path().join("test_exe");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_DEBUG_IR").is_ok() {
        println!("[build_and_run] codegen start");
    }
    ctx.codegen_module(&frontend_output.mir).unwrap();
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_DEBUG_IR").is_ok() {
        println!("[build_and_run] codegen done");
    }

    if std::env::var("GLYPH_DEBUG_IR").is_ok() {
        let ir = ctx.dump_ir();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("std_json_parser_debug.ll"), ir).unwrap();
        if skip_run {
            return 0;
        }
    }

    if skip_run {
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
fn glyph_string_literal(input: &str) -> String {
    let mut out = String::from("\"");
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn map_string_basic() {
    let source = r#"
        from std/map import Map
        from std/enums import Option, Result

        fn main() -> i32 {
          let mut m: Map<String, i32> = Map::new()
          let add_res = m.add(String::from_str("a"), 1)
          let add_code = match add_res {
            Ok(_) => 0,
            Err(_e) => 10,
          }
          if add_code != 0 { ret add_code }
          let g = m.get(String::from_str("a"))
          ret match g {
            Some(v) => if v == 1 { 0 } else { 11 },
            None => 12,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_map_get_nested_value_by_value_no_abort() {
    let source = r#"
        from std/json import JsonValue
        from std/enums import Option
        from std/map import Map

        fn inspect_meta(value: JsonValue) -> i32 {
          ret match value {
            Object(meta) => {
              let flag = meta.get(String::from_str("flag"))
              match flag {
                Some(flag_value) => match flag_value {
                  Bool(b) => if b { 0 } else { 1 },
                  _ => 2,
                },
                None => 3,
              }
            }
            _ => 4,
          }
        }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 100 {
            let mut inner: Map<String, JsonValue> = Map::new()
            let _ = inner.add(String::from_str("flag"), JsonValue::Bool(true))

            let mut outer: Map<String, JsonValue> = Map::new()
            let _ = outer.add(String::from_str("meta"), JsonValue::Object(inner))

            let got = outer.get(String::from_str("meta"))
            let code = match got {
              Some(value) => inspect_meta(value),
              None => 5,
            }
            if code != 0 { ret code }

            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_nested_and_trailing() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/vec import Vec

        from std/json/parser import parse

        fn check_null() -> i32 {
          let r = parse("null")
          ret match r {
            Ok(v) => match v {
              Null => 0,
              _ => 1,
            },
            Err(_e) => 1,
          }
        }

        fn check_nested_array() -> i32 {
          let r = parse("[[1, 2], [3, 4]]")
          ret match r {
            Ok(v) => match v {
              Array(arr) => check_outer_array(arr),
              _ => 1,
            },
            Err(_e) => 1,
          }
        }

        fn check_outer_array(arr0: Vec<JsonValue>) -> i32 {
          let mut arr = arr0
          let right = arr.pop()
          let left = arr.pop()
          let extra = arr.pop()

          ret match extra {
            Some(_v) => 1,
            None => match left {
              Some(lv) => match lv {
                Array(inner) => match right {
                  Some(rv) => match rv {
                    Array(inner2) => {
                      let s0 = check_pair_numbers(inner)
                      let s1 = check_pair_numbers(inner2)
                      if s0 == 0 { s1 } else { 1 }
                    }
                    _ => 1,
                  },
                  None => 1,
                },
                _ => 1,
              },
              None => 1,
            },
          }
        }

        fn check_pair_numbers(arr0: Vec<JsonValue>) -> i32 {
          let mut arr = arr0
          let b = arr.pop()
          let a = arr.pop()
          let extra = arr.pop()

          ret match extra {
            Some(_v) => 1,
            None => match a {
              Some(av) => match av {
                Number(_) => match b {
                  Some(bv) => match bv {
                    Number(_) => 0,
                    _ => 1,
                  },
                  None => 1,
                },
                _ => 1,
              },
              None => 1,
            },
          }
        }

        fn check_nested_object() -> i32 {
          let r = parse("{\"a\": {\"b\": 3}}")
          ret match r {
            Ok(v) => match v {
              Object(obj) => check_object_a_b(obj),
              _ => 11,
            },
            Err(_e) => 101,
          }
        }

        fn check_object_a_b(obj: Map<String, JsonValue>) -> i32 {
          let a_opt = obj.get(String::from_str("a"))
          ret match a_opt {
            Some(a_val) => match a_val {
              Object(inner) => {
                let b_opt = inner.get(String::from_str("b"))
                match b_opt {
                  Some(b_val) => match b_val {
                    Number(_) => 0,
                    _ => 13,
                  },
                  None => 12,
                }
              }
              _ => 14,
            },
            None => 15,
          }
        }

        fn check_trailing_rejected() -> i32 {
          let r = parse("null x")
          ret match r {
            Ok(_v) => 1,
            Err(_e) => 0,
          }
        }

        fn main() -> i32 {
          let r = check_nested_object()
          ret r
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_string_escapes() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/string import byte_at

        from std/json/parser import parse

        fn hash_str(s: str) -> i64 {
          let mut i: usize = 0
          let len = s.len()
          let mut h: i64 = 0
          while i < len {
            let b = byte_at(s, i)
            h = h * 31 + b
            i = i + 1
          }
          ret h
        }

        fn check_hash(input: &str, expected: i64) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              String(s) => if hash_str(s) == expected { 0 } else { 1 },
              _ => 1,
            },
            Err(_e) => 2,
          }
        }

        fn check_err(input: &str) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(_v) => 1,
            Err(_e) => 0,
          }
        }

        fn check_object_text() -> i32 {
          let r = parse("{\"text\":\"hi\\nthere\"}")
          ret match r {
            Ok(v) => match v {
              Object(obj) => {
                let t = obj.get(String::from_str("text"))
                match t {
                  Some(tv) => match tv {
                    String(s) => if hash_str(s) == 2954896372955 { 0 } else { 1 },
                    _ => 1,
                  },
                  None => 1,
                }
              }
              _ => 1,
            },
            Err(_e) => 1,
          }
        }

        fn main() -> i32 {
          let r = parse("\"ab\"")
          ret match r {
            Ok(v) => match v {
              String(s) => {
                let s0: str = s
                let len = s0.len()
                if len == 2 {
                  let b0 = byte_at(s0, 0)
                  let b1 = byte_at(s0, 1)
                  if b0 == 97 && b1 == 98 { 0 } else { 21 }
                } else {
                  if len == 4 {
                    let b0 = byte_at(s0, 0)
                    let b1 = byte_at(s0, 1)
                    let b2 = byte_at(s0, 2)
                    let b3 = byte_at(s0, 3)
                    if b0 == 34 && b1 == 97 && b2 == 98 && b3 == 34 { 40 } else { 41 }
                  } else {
                    if len == 6 {
                      let b0 = byte_at(s0, 0)
                      let b1 = byte_at(s0, 1)
                      let b2 = byte_at(s0, 2)
                      let b3 = byte_at(s0, 3)
                      let b4 = byte_at(s0, 4)
                      let b5 = byte_at(s0, 5)
                      if b0 == 92 && b1 == 34 && b2 == 97 && b3 == 98 && b4 == 92 && b5 == 34 { 60 } else { 61 }
                    } else {
                      30
                    }
                  }
                }
              }
              _ => 22,
            },
            Err(_e) => 222,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_hardening_valid_matrix() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/string import byte_at
        from std/vec import Vec

        from std/json/parser import parse

        fn hash_str_mod(s: str) -> i64 {
          let mut i: usize = 0
          let len = s.len()
          let mut h: i64 = 0
          while i < len {
            let b = byte_at(s, i)
            h = (h * 131 + b) % 1000000007
            i = i + 1
          }
          ret h
        }

        fn expect_ok(input: &str) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(_v) => 0,
            Err(_e) => 1,
          }
        }

        fn expect_bool(input: &str, expected: bool) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              Bool(b) => if b == expected { 0 } else { 1 },
              _ => 2,
            },
            Err(_e) => 3,
          }
        }

        fn expect_number(input: &str) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              Number(_n) => 0,
              _ => 1,
            },
            Err(_e) => 2,
          }
        }

        fn expect_string_hash(input: &str, expected: i64) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              String(s) => if hash_str_mod(s) == expected { 0 } else { 1 },
              _ => 2,
            },
            Err(_e) => 3,
          }
        }

        fn check_meta(meta_value: JsonValue) -> i32 {
          ret match meta_value {
            Object(meta_obj) => {
              let ok_opt = meta_obj.get(String::from_str("ok"))
              let count_opt = meta_obj.get(String::from_str("count"))
              match ok_opt {
                Some(ok_val) => match ok_val {
                  Bool(b) => if b {
                    match count_opt {
                      Some(count_val) => match count_val {
                        Number(_n) => 0,
                        _ => 1,
                      },
                      None => 2,
                    }
                  } else {
                    3
                  },
                  _ => 4,
                },
                None => 5,
              }
            }
            _ => 6,
          }
        }

        fn check_data(data_value: JsonValue) -> i32 {
          ret match data_value {
            Array(values0) => {
              let mut values = values0
              let v3 = values.pop()
              let v2 = values.pop()
              let v1 = values.pop()
              let v0 = values.pop()
              let extra = values.pop()

              match extra {
                Some(_e) => 1,
                None => match v0 {
                  Some(a0) => match a0 {
                    Null => match v1 {
                      Some(a1) => match a1 {
                        Bool(b1) => if b1 == false {
                          match v2 {
                            Some(a2) => match a2 {
                              Number(_n) => match v3 {
                                Some(a3) => match a3 {
                                  String(s3) => if hash_str_mod(s3) == 120 { 0 } else { 2 },
                                  _ => 3,
                                },
                                None => 4,
                              },
                              _ => 5,
                            },
                            None => 6,
                          }
                        } else {
                          7
                        },
                        _ => 8,
                      },
                      None => 9,
                    },
                    _ => 10,
                  },
                  None => 11,
                },
              }
            }
            _ => 12,
          }
        }

        fn expect_complex_object() -> i32 {
          let r = parse("{\"meta\":{\"ok\":true,\"count\":3},\"data\":[null,false,1.5e2,\"x\"]}")
          ret match r {
            Ok(v) => match v {
              Object(obj) => {
                let meta_opt = obj.get(String::from_str("meta"))
                let data_opt = obj.get(String::from_str("data"))
                match meta_opt {
                  Some(meta_value) => match data_opt {
                    Some(data_value) => {
                      let meta_status = check_meta(meta_value)
                      if meta_status != 0 { meta_status } else { check_data(data_value) }
                    }
                    None => 13,
                  },
                  None => 14,
                }
              }
              _ => 15,
            },
            Err(_e) => 16,
          }
        }

        fn main() -> i32 {
          let c0 = expect_ok("   null   ")
          if c0 != 0 { ret 10 }

          let c1 = expect_bool("true", true)
          if c1 != 0 { ret 11 }
          let c2 = expect_bool("false", false)
          if c2 != 0 { ret 12 }

          let c3 = expect_number("0")
          if c3 != 0 { ret 13 }
          let c4 = expect_number("-0")
          if c4 != 0 { ret 14 }
          let c5 = expect_number("12345")
          if c5 != 0 { ret 15 }
          let c6 = expect_number("-98")
          if c6 != 0 { ret 16 }
          let c7 = expect_number("0.5")
          if c7 != 0 { ret 17 }
          let c8 = expect_number("1e10")
          if c8 != 0 { ret 18 }
          let c9 = expect_number("1E-3")
          if c9 != 0 { ret 19 }
          let c10 = expect_number("-2.5e+7")
          if c10 != 0 { ret 20 }

          let c11 = expect_ok("[1, true, null, \"x\", {\"k\":[false]}]")
          if c11 != 0 { ret 21 }
          let c12 = expect_ok("{\"a\":1,\"b\":[2,3],\"c\":{\"d\":4}}")
          if c12 != 0 { ret 22 }

          let c13 = expect_string_hash("\"\\u0041\"", 65)
          if c13 != 0 { ret 23 }
          let c17 = expect_string_hash("\"line\\nfeed\\tok\"", 266679051)
          if c17 != 0 { ret 27 }

          let c18 = expect_complex_object()
          if c18 != 0 { ret 28 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_hardening_invalid_matrix() {
    let source = r#"
        from std/json import JsonValue, ParseError, ParseResult
        from std/string import byte_at
        from std/json/parser import parse

        fn hash_str_mod(s: str) -> i64 {
          let mut i: usize = 0
          let len = s.len()
          let mut h: i64 = 0
          while i < len {
            let b = byte_at(s, i)
            h = (h * 131 + b) % 1000000007
            i = i + 1
          }
          ret h
        }

        fn expect_err_pos_msg(input: &str, expected_pos: usize, expected_msg_hash: i64) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(_v) => 1,
            Err(e) => {
              let pos = e.position
              let msg: str = e.message
              let msg_hash = hash_str_mod(msg)
              if pos != expected_pos {
                2
              } else {
                if msg_hash == expected_msg_hash { 0 } else { 3 }
              }
            },
          }
        }

        fn main() -> i32 {
          let c0 = expect_err_pos_msg("null x", 5, 316366985)
          if c0 != 0 { ret 10 }
          let c1 = expect_err_pos_msg("true false", 5, 316366985)
          if c1 != 0 { ret 11 }

          let c2 = expect_err_pos_msg("[1,]", 3, 207243600)
          if c2 != 0 { ret 12 }
          let c3 = expect_err_pos_msg("{\"a\":1,}", 7, 515054770)
          if c3 != 0 { ret 13 }
          let c4 = expect_err_pos_msg("{\"a\" 1}", 5, 82097310)
          if c4 != 0 { ret 14 }
          let c5 = expect_err_pos_msg("{\"a\":1 \"b\":2}", 7, 872308585)
          if c5 != 0 { ret 15 }
          let c6 = expect_err_pos_msg("[1 2]", 3, 872304393)
          if c6 != 0 { ret 16 }

          let c7 = expect_err_pos_msg("1.", 2, 600451706)
          if c7 != 0 { ret 17 }
          let c8 = expect_err_pos_msg("1e+", 3, 221197222)
          if c8 != 0 { ret 18 }
          let c9 = expect_err_pos_msg("-", 1, 943194561)
          if c9 != 0 { ret 19 }
          let c10 = expect_err_pos_msg("+1", 0, 207243600)
          if c10 != 0 { ret 20 }
          let c11 = expect_err_pos_msg("01", 1, 297189801)
          if c11 != 0 { ret 21 }
          let c12 = expect_err_pos_msg("-01", 2, 297189801)
          if c12 != 0 { ret 22 }

          let c13 = expect_err_pos_msg("\"\\x\"", 1, 444696919)
          if c13 != 0 { ret 23 }
          let c14 = expect_err_pos_msg("\"\\u12\"", 1, 55332458)
          if c14 != 0 { ret 24 }
          let c15 = expect_err_pos_msg("\"\\uD800\"", 1, 960656384)
          if c15 != 0 { ret 25 }
          let c16 = expect_err_pos_msg("\"\\uDC00\"", 1, 960656384)
          if c16 != 0 { ret 26 }
          let c17 = expect_err_pos_msg("\"\\uD800\\u0061\"", 1, 960656384)
          if c17 != 0 { ret 27 }
          let c18 = expect_err_pos_msg("\"\\u0000\"", 1, 260413370)
          if c18 != 0 { ret 28 }

          let c19 = expect_err_pos_msg("[", 1, 47974317)
          if c19 != 0 { ret 29 }
          let c20 = expect_err_pos_msg("{\"a\":", 5, 47974317)
          if c20 != 0 { ret 30 }
          let c21 = expect_err_pos_msg("\"unterminated", 0, 463322056)
          if c21 != 0 { ret 31 }
          let c22 = expect_err_pos_msg("\"a\nb\"", 2, 77597555)
          if c22 != 0 { ret 32 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_hardening_deep_nesting() {
    let depth = 96usize;
    let mut json = String::new();
    for _ in 0..depth {
        json.push('[');
    }
    json.push('0');
    for _ in 0..depth {
        json.push(']');
    }
    let json_literal = glyph_string_literal(&json);

    let source = format!(
        r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/vec import Vec
        from std/json/parser import parse

        fn unwrap_depth(v: JsonValue, n: i32) -> i32 {{
          if n == 0 {{
            ret match v {{
              Number(_x) => 0,
              _ => 1,
            }}
          }}

          ret match v {{
            Array(values0) => {{
              let mut values = values0
              let child = values.pop()
              let extra = values.pop()
              match extra {{
                Some(_e) => 2,
                None => match child {{
                  Some(next) => unwrap_depth(next, n - 1),
                  None => 3,
                }},
              }}
            }}
            _ => 4,
          }}
        }}

        fn main() -> i32 {{
          let r = parse({})
          ret match r {{
            Ok(v) => unwrap_depth(v, {}),
            Err(_e) => 5,
          }}
        }}
    "#,
        json_literal, depth
    );

    assert_eq!(build_and_run_exit_code(&source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_drop_object_no_heap_corruption() {
    let source = r#"
        from std/json import JsonValue, ParseResult
        from std/json/parser import parse

        fn parse_and_drop() -> i32 {
          let r = parse("{\"phase\": \"implementation\", \"version\": 1, \"name\": \"hello\"}")
          ret match r {
            Ok(_v) => 0,
            Err(_e) => 1,
          }
        }

        fn main() -> i32 {
          let a = parse_and_drop()
          let _b = String::from_str("post-drop allocation")
          let _c = String::from_str("second post-drop allocation")
          ret a
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_drop_nested_object_no_heap_corruption() {
    let source = r#"
        from std/json import JsonValue, ParseResult
        from std/json/parser import parse

        fn main() -> i32 {
          let r = parse("{\"version\": 1, \"project_root\": \"snake_e\", \"phase\": \"implementation\", \"opencode\": {\"bin\": \"/usr/bin/opencode\"}, \"run\": {\"max_iterations\": 200, \"iteration\": 1}}")
          let exit = match r {
            Ok(_v) => 0,
            Err(_e) => 1,
          }
          let _probe = String::from_str("heap probe after json drop")
          ret exit
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_drop_string_escape_no_heap_corruption() {
    let source = r#"
        from std/json import JsonValue, ParseResult
        from std/json/parser import parse

        fn main() -> i32 {
          let r = parse("\"hello\\nworld\\ttab\\\"quote\"")
          let exit = match r {
            Ok(_v) => 0,
            Err(_e) => 1,
          }
          let _probe = String::from_str("heap probe")
          ret exit
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_drop_loop_no_heap_corruption() {
    let source = r#"
        from std/json import JsonValue, ParseResult
        from std/json/parser import parse

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 10 {
            let r = parse("{\"key\": \"value\", \"n\": 42}")
            let _ = match r {
              Ok(_v) => 0,
              Err(_e) => 1,
            }
            let _probe = String::from_str("loop probe")
            i = i + 1
          }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_unicode() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/string import byte_at
        from std/json/parser import parse

        fn check_bytes2(input: &str, b0: u8, b1: u8) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              String(s) => {
                let sr: str = s
                if sr.len() != 2 { ret 1 }
                if byte_at(sr, 0) != b0 { ret 2 }
                if byte_at(sr, 1) != b1 { ret 3 }
                0
              },
              _ => 4,
            },
            Err(_e) => 5,
          }
        }

        fn check_bytes3(input: &str, b0: u8, b1: u8, b2: u8) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              String(s) => {
                let sr: str = s
                if sr.len() != 3 { ret 1 }
                if byte_at(sr, 0) != b0 { ret 2 }
                if byte_at(sr, 1) != b1 { ret 3 }
                if byte_at(sr, 2) != b2 { ret 4 }
                0
              },
              _ => 5,
            },
            Err(_e) => 6,
          }
        }

        fn check_bytes4(input: &str, b0: u8, b1: u8, b2: u8, b3: u8) -> i32 {
          let r = parse(input)
          ret match r {
            Ok(v) => match v {
              String(s) => {
                let sr: str = s
                if sr.len() != 4 { ret 1 }
                if byte_at(sr, 0) != b0 { ret 2 }
                if byte_at(sr, 1) != b1 { ret 3 }
                if byte_at(sr, 2) != b2 { ret 4 }
                if byte_at(sr, 3) != b3 { ret 5 }
                0
              },
              _ => 6,
            },
            Err(_e) => 7,
          }
        }

        fn main() -> i32 {
          // U+00DF = ß (2-byte UTF-8: 0xC3 0x9F)
          let c0 = check_bytes2("\"\\u00DF\"", 195, 159)
          if c0 != 0 { ret 10 + c0 }

          // U+4F60 = 你 (3-byte UTF-8: 0xE4 0xBD 0xA0)
          let c1 = check_bytes3("\"\\u4F60\"", 228, 189, 160)
          if c1 != 0 { ret 20 + c1 }

          // U+1F603 = surrogate pair (4-byte UTF-8: 0xF0 0x9F 0x98 0x83)
          let c2 = check_bytes4("\"\\uD83D\\uDE03\"", 240, 159, 152, 131)
          if c2 != 0 { ret 30 + c2 }

          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_accessors() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/vec import Vec
        from std/json/parser import parse, json_get_string, json_get_number, json_get_bool, json_is_null, json_get_array, json_get_object

        fn test_get_string() -> i32 {
          let r = parse("\"hello\"")
          ret match r {
            Ok(v) => {
              let s = json_get_string(v)
              match s {
                Some(ss) => {
                  let sr: str = ss
                  if sr.len() != 5 { 11 } else { 0 }
                },
                None => 12,
              }
            },
            Err(_e) => 10,
          }
        }

        fn test_get_number() -> i32 {
          let r = parse("42")
          ret match r {
            Ok(v) => {
              let n = json_get_number(v)
              match n {
                Some(_nn) => 0,
                None => 21,
              }
            },
            Err(_e) => 20,
          }
        }

        fn test_get_bool() -> i32 {
          let r = parse("true")
          ret match r {
            Ok(v) => {
              let b = json_get_bool(v)
              match b {
                Some(bb) => if bb == false { 31 } else { 0 },
                None => 32,
              }
            },
            Err(_e) => 30,
          }
        }

        fn test_is_null() -> i32 {
          let r = parse("null")
          ret match r {
            Ok(v) => if json_is_null(v) == false { 41 } else { 0 },
            Err(_e) => 40,
          }
        }

        fn test_get_string_wrong_type() -> i32 {
          let r = parse("42")
          ret match r {
            Ok(v) => {
              let s = json_get_string(v)
              match s {
                Some(_ss) => 51,
                None => 0,
              }
            },
            Err(_e) => 50,
          }
        }

        fn test_get_array() -> i32 {
          let r = parse("[1,2]")
          ret match r {
            Ok(v) => {
              let a = json_get_array(v)
              match a {
                Some(arr) => if arr.len() != 2 { 61 } else { 0 },
                None => 62,
              }
            },
            Err(_e) => 60,
          }
        }

        fn test_get_object() -> i32 {
          let r = parse("{\"a\":1}")
          ret match r {
            Ok(v) => {
              let o = json_get_object(v)
              match o {
                Some(obj) => {
                  let has_a = obj.has(String::from_str("a"))
                  if has_a == false { 71 } else { 0 }
                },
                None => 72,
              }
            },
            Err(_e) => 70,
          }
        }

        fn main() -> i32 {
          let c0 = test_get_string()
          if c0 != 0 { ret c0 }
          let c1 = test_get_number()
          if c1 != 0 { ret c1 }
          let c2 = test_get_bool()
          if c2 != 0 { ret c2 }
          let c3 = test_is_null()
          if c3 != 0 { ret c3 }
          let c4 = test_get_string_wrong_type()
          if c4 != 0 { ret c4 }
          let c5 = test_get_array()
          if c5 != 0 { ret c5 }
          let c6 = test_get_object()
          if c6 != 0 { ret c6 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_json_parser_stringify_roundtrip() {
    let source = r#"
        from std/enums import Option
        from std/json import JsonValue, ParseResult
        from std/map import Map
        from std/vec import Vec
        from std/string import byte_at
        from std/json/parser import parse, stringify, json_is_null, json_get_bool, json_get_number, json_get_string, json_get_array, json_get_object

        fn str_eq(a: str, b: str) -> bool {
          let alen = a.len()
          let blen = b.len()
          if alen != blen { ret false }
          let mut i: usize = 0
          while i < alen {
            if byte_at(a, i) != byte_at(b, i) { ret false }
            i = i + 1
          }
          ret true
        }

        fn test_stringify_null() -> i32 {
          let r = parse("null")
          ret match r {
            Ok(v) => {
              let s: str = stringify(v)
              if str_eq(s, "null") { 0 } else { 11 }
            },
            Err(_e) => 10,
          }
        }

        fn test_stringify_true() -> i32 {
          let r = parse("true")
          ret match r {
            Ok(v) => {
              let s: str = stringify(v)
              if str_eq(s, "true") { 0 } else { 21 }
            },
            Err(_e) => 20,
          }
        }

        fn test_stringify_false() -> i32 {
          let r = parse("false")
          ret match r {
            Ok(v) => {
              let s: str = stringify(v)
              if str_eq(s, "false") { 0 } else { 31 }
            },
            Err(_e) => 30,
          }
        }

        fn test_stringify_number() -> i32 {
          let r = parse("42")
          ret match r {
            Ok(v) => {
              let s = stringify(v)
              let r2 = parse(s)
              match r2 {
                Ok(v2) => {
                  let n = json_get_number(v2)
                  match n {
                    Some(_nn) => 0,
                    None => 42,
                  }
                },
                Err(_e2) => 41,
              }
            },
            Err(_e) => 40,
          }
        }

        fn test_stringify_string_escape() -> i32 {
          let r = parse("\"hello\\nworld\"")
          ret match r {
            Ok(v) => {
              let s = stringify(v)
              let r2 = parse(s)
              match r2 {
                Ok(v2) => {
                  let gs = json_get_string(v2)
                  match gs {
                    Some(ss) => {
                      let sr: str = ss
                      if sr.len() != 11 { 52 } else { 0 }
                    },
                    None => 53,
                  }
                },
                Err(_e2) => 51,
              }
            },
            Err(_e) => 50,
          }
        }

        fn test_stringify_array() -> i32 {
          let r = parse("[1,2,3]")
          ret match r {
            Ok(v) => {
              let s = stringify(v)
              let r2 = parse(s)
              match r2 {
                Ok(v2) => {
                  let a = json_get_array(v2)
                  match a {
                    Some(arr) => if arr.len() != 3 { 62 } else { 0 },
                    None => 63,
                  }
                },
                Err(_e2) => 61,
              }
            },
            Err(_e) => 60,
          }
        }

        fn test_stringify_object() -> i32 {
          let r = parse("{\"a\":1}")
          ret match r {
            Ok(v) => {
              let s = stringify(v)
              let r2 = parse(s)
              match r2 {
                Ok(v2) => {
                  let o = json_get_object(v2)
                  match o {
                    Some(obj) => {
                      let has_a = obj.has(String::from_str("a"))
                      if has_a == false { 72 } else { 0 }
                    },
                    None => 73,
                  }
                },
                Err(_e2) => 71,
              }
            },
            Err(_e) => 70,
          }
        }

        fn main() -> i32 {
          let c0 = test_stringify_null()
          if c0 != 0 { ret c0 }
          let c1 = test_stringify_true()
          if c1 != 0 { ret c1 }
          let c2 = test_stringify_false()
          if c2 != 0 { ret c2 }
          let c3 = test_stringify_number()
          if c3 != 0 { ret c3 }
          let c4 = test_stringify_string_escape()
          if c4 != 0 { ret c4 }
          let c5 = test_stringify_array()
          if c5 != 0 { ret c5 }
          let c6 = test_stringify_object()
          if c6 != 0 { ret c6 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

/// Regression guard for GLYPH-9: `stdlib.rs` must embed the full
/// `crates/glyph-frontend/src/stdlib/json/parser.glyph` (shipped via
/// `build.rs` into `OUT_DIR/json_parser_source.txt`), never the historical
/// `parser_inline.glyph` stub. That stub defined only a single `parse`
/// function which unconditionally returned `Ok(JsonValue::Null())` no
/// matter the input; it has since been deleted, but this test pins the
/// *mechanism* (what `glyph_frontend::std_modules()` actually registers
/// for `"std/json/parser"`) so a future change can't silently reintroduce
/// a stub without this test catching it. No codegen/linking is needed:
/// this only inspects the parsed AST that stdlib.rs hands to the rest of
/// the compiler.
#[test]
fn std_json_parser_embeds_full_parser_not_stub() {
    let modules = glyph_frontend::std_modules();
    let parser_module = modules
        .get("std/json/parser")
        .expect("std/json/parser module must be registered by stdlib.rs");

    let fn_names: std::collections::HashSet<&str> = parser_module
        .items
        .iter()
        .filter_map(|item| match item {
            glyph_core::ast::Item::Function(f) => Some(f.name.0.as_str()),
            _ => None,
        })
        .collect();

    // Present only in the full parser.glyph; the historical stub defined
    // nothing but `parse`.
    let full_parser_only = [
        "json_get_string",
        "json_get_number",
        "json_get_bool",
        "json_is_null",
        "json_get_array",
        "json_get_object",
        "stringify",
        "stringify_string",
        "stringify_array",
        "stringify_object",
    ];
    for name in full_parser_only {
        assert!(
            fn_names.contains(name),
            "std/json/parser is missing `{name}` - the embedded source \
             looks like the historical stub, not the full parser.glyph"
        );
    }

    assert!(
        fn_names.contains("parse"),
        "std/json/parser must still export `parse`"
    );
}
