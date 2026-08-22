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
fn early_return_if_then_runtime() {
    let source = r#"
        from std/string import byte_at

        fn main() -> i32 {
          let s = String::from_str("abc")
          if false { ret 9 }
          let s_ref: str = s
          if s_ref.len() != 3 { ret 1 }
          if byte_at(s_ref, 2) != 99 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn early_return_if_else_runtime() {
    let source = r#"
        from std/string import byte_at

        fn main() -> i32 {
          let s = String::from_str("xy")
          if true { } else { ret 7 }
          let s_ref: str = s
          if s_ref.len() != 2 { ret 1 }
          if byte_at(s_ref, 0) != 120 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn early_return_nested_if_runtime() {
    let source = r#"
        from std/string import byte_at

        fn main() -> i32 {
          let s = String::from_str("nest")
          if true {
            if false { ret 3 }
          }
          let s_ref: str = s
          if s_ref.len() != 4 { ret 1 }
          if byte_at(s_ref, 3) != 116 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn early_return_if_expr_runtime() {
    let source = r#"
        from std/string import byte_at

        fn main() -> i32 {
          let s = String::from_str("ok")
          let v = if false { ret 4 } else { 2 }
          if v != 2 { ret 1 }
          let s_ref: str = s
          if byte_at(s_ref, 1) != 107 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn early_return_match_arm_runtime() {
    let source = r#"
        from std/string import byte_at

        enum Flag { A, B }

        fn main() -> i32 {
          let s = String::from_str("hi")
          let flag = B()
          let v = match flag {
            A => { ret 3 }
            B => 1
          }
          if v != 1 { ret 1 }
          let s_ref: str = s
          if s_ref.len() != 2 { ret 2 }
          if byte_at(s_ref, 1) != 105 { ret 3 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn try_option_some_and_none_runtime() {
    let source = r#"
        from std/enums import Option

        fn some_value() -> Option<i32> {
          ret Some(41)
        }

        fn none_value() -> Option<i32> {
          ret None()
        }

        fn use_some() -> Option<i32> {
          let value = some_value()?
          ret Some(value + 1)
        }

        fn use_none() -> Option<i32> {
          let value = none_value()?
          ret Some(value + 1)
        }

        fn main() -> i32 {
          let a = use_some()
          let first = match a {
            Some(v) => if v == 42 { 0 } else { 1 },
            None => 2,
          }
          if first != 0 { ret first }

          let b = use_none()
          ret match b {
            Some(_v) => 3,
            None => 0,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn try_result_ok_and_err_runtime() {
    let source = r#"
        from std/enums import Result

        fn ok_value() -> Result<i32, String> {
          ret Ok(40)
        }

        fn err_value() -> Result<i32, String> {
          ret Err(String::from_str("bad"))
        }

        fn use_ok() -> Result<i32, String> {
          let value = ok_value()?
          ret Ok(value + 2)
        }

        fn use_err() -> Result<i32, String> {
          let value = err_value()?
          ret Ok(value + 2)
        }

        fn main() -> i32 {
          let ok_result = use_ok()
          let ok_code = match ok_result {
            Ok(v) => if v == 42 { 0 } else { 1 },
            Err(_e) => 2,
          }
          if ok_code != 0 { ret ok_code }

          let err_result = use_err()
          ret match err_result {
            Ok(_v) => 3,
            Err(e) => {
              let msg: str = e
              if msg.len() == 3 { 0 } else { 4 }
            },
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
