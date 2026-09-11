#![cfg(all(feature = "codegen", unix))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use glyph_backend::codegen::CodegenContext;
use glyph_backend::linker::{Linker, LinkerOptions};
use glyph_frontend::{FrontendOptions, compile_source};

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<ThreadEntry>,
    ) -> i32;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_test_fail_next(operation: i32, error_code: i32) -> i32;
}

const TEST_FAIL_CREATE: i32 = 2;
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_thread_runtime() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
}

fn runtime_symbols() -> HashMap<String, u64> {
    HashMap::from([
        (
            "glyph_thread_spawn".into(),
            glyph_thread_spawn as *const () as usize as u64,
        ),
        (
            "glyph_thread_join".into(),
            glyph_thread_join as *const () as usize as u64,
        ),
        (
            "glyph_thread_detach".into(),
            glyph_thread_detach as *const () as usize as u64,
        ),
    ])
}

const SPAWN_JOIN: &str = r#"
import spawn from std/thread

fn main() -> i32 {
  let message: String = String::from_str("drop on failed creation")
  let task: FnOnce<(), ()> = move () -> {
    let length = message.len()
  }
  let outcome = spawn(task)
  ret match outcome {
    Ok(handle) => {
      let joined = handle.join()
      42
    },
    Err(_error) => 1,
  }
}
"#;

#[test]
fn thread_source_spawns_and_joins_in_jit() {
    let _guard = lock_thread_runtime();
    let output = compile(SPAWN_JOIN);
    let mut codegen = CodegenContext::new("thread_source_jit").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
}

#[test]
fn thread_source_links_and_executes_as_a_native_object() {
    let _guard = lock_thread_runtime();
    let output = compile(SPAWN_JOIN);
    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("thread.o");
    let executable = temp.path().join("thread");
    let mut codegen = CodegenContext::new("thread_source_object").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object],
            link_libs: vec![],
            link_search_paths: vec![],
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();
    assert_eq!(
        std::process::Command::new(executable)
            .status()
            .unwrap()
            .code(),
        Some(42)
    );
}

#[test]
fn source_creation_failure_returns_err_and_drops_the_task() {
    let _guard = lock_thread_runtime();
    let source = r#"
import spawn from std/thread

fn main() -> i32 {
  let message: String = String::from_str("drop on failed creation")
  let task: FnOnce<(), ()> = move () -> {
    let length = message.len()
  }
  let outcome = spawn(task)
  ret match outcome {
    Ok(_handle) => 1,
    Err(_error) => 42,
  }
}
"#;
    let output = compile(source);
    let mut codegen = CodegenContext::new("thread_source_create_failure").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_CREATE, libc::EAGAIN) },
        0
    );
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
}

#[test]
fn source_detach_nested_and_many_tasks_compile_and_run_without_sleeps() {
    let _guard = lock_thread_runtime();
    let mut source = r#"
import spawn from std/thread

fn leaf() {}

fn nested() {
  let child = spawn(leaf)
}

fn main() -> i32 {
"#
    .to_string();
    for index in 0..32 {
        let task = if index == 0 { "nested" } else { "leaf" };
        source.push_str(&format!("  let task_{index} = spawn({task})\n"));
    }
    for index in 1..32 {
        source.push_str(&format!(
            "  let joined_{index} = match task_{index} {{\n    Ok(handle) => {{ let status = handle.join() 0 }},\n    Err(_error) => 1,\n  }}\n"
        ));
    }
    source.push_str(
        r#"  ret match task_0 {
    Ok(handle) => {
      let detached = handle.detach()
      42
    },
    Err(_error) => 1,
  }
}
"#,
    );
    let output = compile(&source);
    let mut codegen = CodegenContext::new("thread_source_nested_many").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
}

#[test]
fn source_thread_moves_an_owned_string_capture_and_cleans_it_once() {
    let _guard = lock_thread_runtime();
    let source = r#"
import spawn from std/thread

fn main() -> i32 {
  let message: String = String::from_str("owned capture")
  let task: FnOnce<(), ()> = move () -> {
    let length = message.len()
  }
  let outcome = spawn(task)
  ret match outcome {
    Ok(handle) => {
      let joined = handle.join()
      42
    },
    Err(_error) => 1,
  }
}
"#;
    let output = compile(source);
    let mut codegen = CodegenContext::new("thread_source_owned_capture").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
}

#[test]
fn detached_source_thread_does_not_keep_the_native_process_alive() {
    let _guard = lock_thread_runtime();
    let source = r#"
import spawn from std/thread

fn worker() {}

fn main() -> i32 {
  let outcome = spawn(worker)
  ret match outcome {
    Ok(handle) => {
      let detached = handle.detach()
      42
    },
    Err(_error) => 1,
  }
}
"#;
    let output = compile(source);
    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("thread_detach.o");
    let executable = temp.path().join("thread_detach");
    let mut codegen = CodegenContext::new("thread_source_detach_object").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object],
            link_libs: vec![],
            link_search_paths: vec![],
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();
    assert_eq!(
        std::process::Command::new(executable)
            .status()
            .unwrap()
            .code(),
        Some(42)
    );
}
