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
fn std_time_human_readable_format() {
    let source = r#"
        from std/time import get_timestamp, Timestamp, Error
        from std/string import byte_at
        from std/enums import Result

        fn main() -> i32 {
          let ts = get_timestamp()
          let res: Result<String, Error> = ts.to_human_readable()
          ret match res {
            Ok(s) => {
              let s0: str = s
              if s0.len() != 19 { ret 10 }
              if byte_at(s0, 2) != 47 { ret 11 }
              if byte_at(s0, 5) != 47 { ret 12 }
              if byte_at(s0, 10) != 32 { ret 13 }
              if byte_at(s0, 13) != 58 { ret 14 }
              if byte_at(s0, 16) != 58 { ret 15 }
              ret 0
            },
            Err(_e) => 1,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_sleep_ms_zero() {
    let source = r#"
        from std/time import sleep_ms

        fn main() -> i32 {
          let _r = sleep_ms(0)
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_sleep_us_zero() {
    let source = r#"
        from std/time import sleep_us

        fn main() -> i32 {
          let _r = sleep_us(0)
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_sleep_ms_small() {
    let source = r#"
        from std/time import sleep_ms

        fn main() -> i32 {
          let _r = sleep_ms(10)
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_sleep_us_small() {
    let source = r#"
        from std/time import sleep_us

        fn main() -> i32 {
          let _r = sleep_us(5000)
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_monotonic_clock_measures_sleep() {
    let source = r#"
        from std/time import Instant, now_monotonic, sleep_ms

        fn main() -> i32 {
          let start = now_monotonic()
          let _ = sleep_ms(50)
          let elapsed = start.elapsed_ms()
          if elapsed < 45 { ret 1 }
          if elapsed > 5000 { ret 2 }
          let ns = start.elapsed_ns()
          if ns < 45000000 { ret 3 }
          let us = start.elapsed_us()
          if us < 45000 { ret 4 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_time_monotonic_is_nondecreasing() {
    let source = r#"
        from std/time import Instant, now_monotonic

        fn main() -> i32 {
          let a = now_monotonic()
          let mut i: i32 = 0
          let mut spin: u64 = 0
          while i < 1000 {
            spin = spin + 1
            i = i + 1
          }
          let b = now_monotonic()
          if b.as_nanos() < a.as_nanos() { ret 1 }
          if a.as_nanos() == 0 { ret 2 }
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
