#![cfg(all(feature = "codegen", unix))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
use glyph_frontend::{FrontendOptions, compile_source};

#[repr(C)]
struct GlyphThreadScope {
    _private: [u8; 0],
}

#[repr(C)]
struct GlyphScopedThread {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);
type ThreadResultEntry = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type DropResult = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    fn glyph_thread_scope_create(out: *mut *mut GlyphThreadScope) -> i32;
    fn glyph_thread_scope_spawn(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
    ) -> i32;
    fn glyph_thread_scope_spawn_result(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        result_size: usize,
        drop_result: Option<DropResult>,
    ) -> i32;
    fn glyph_thread_scope_join(handle: *mut *mut GlyphScopedThread) -> i32;
    fn glyph_thread_scope_join_result(
        handle: *mut *mut GlyphScopedThread,
        out_result: *mut c_void,
    ) -> i32;
    fn glyph_thread_scope_drain(scope: *mut GlyphThreadScope) -> i32;
    fn glyph_thread_scope_drain_or_abort(scope: *mut GlyphThreadScope);
    fn glyph_thread_scope_join_all(scope: *mut *mut GlyphThreadScope) -> i32;
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

const SCOPED_BORROWED_SOURCE: &str = r#"
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums
import File from std/io

fn main() -> i32 {
  let base: i32 = 40
  let joined_scope: Result<Result<String, std::thread::ThreadError>, std::thread::ThreadError> =
    scope((thread_scope: Scope) -> {
    let local: i32 = 1
    let joined_task: Fn<(), String> = () -> {
      if base + local + 1 == 42 {
        String::from_str("joined")
      } else {
        String::from_str("wrong")
      }
    }
    let joined_handle: ScopedJoinHandle<String> = thread_scope.spawn(joined_task)?
    ret joined_handle.join()
  })
  let joined: i32 = match joined_scope {
    Ok(result) => match result {
      Ok(value) => if value.len() == 6 { 42 } else { 12 },
      Err(_error) => { ret 11 },
    },
    Err(_error) => { ret 10 },
  }

  let drained = scope((thread_scope: Scope) -> {
    let expected: i32 = 42
    let unjoined_task: Fn<(), String> = () -> {
      if expected != 42 { ret String::from_str("wrong") }
      let status = match File::create("__MARKER_PATH__") {
        Ok(file) => {
          let written = file.write_string(String::from_str("drained"))
          let closed = file.close()
          expected
        },
        Err(_error) => 0,
      }
      ret if status == 42 {
        String::from_str("unclaimed")
      } else {
        String::from_str("wrong")
      }
    }
    let pending: Result<ScopedJoinHandle<String>, ThreadError> =
      thread_scope.spawn(unjoined_task)
    ret 42
  })

  ret match drained {
    Ok(value) => if joined == 42 { value } else { 12 },
    Err(_error) => 13,
  }
}
"#;

fn source_with_marker(marker: &std::path::Path) -> String {
    let escaped = marker
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    SCOPED_BORROWED_SOURCE.replace("__MARKER_PATH__", &escaped)
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
            "glyph_thread_scope_create".into(),
            glyph_thread_scope_create as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_spawn".into(),
            glyph_thread_scope_spawn as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_spawn_result".into(),
            glyph_thread_scope_spawn_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join".into(),
            glyph_thread_scope_join as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join_result".into(),
            glyph_thread_scope_join_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_drain".into(),
            glyph_thread_scope_drain as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_drain_or_abort".into(),
            glyph_thread_scope_drain_or_abort as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join_all".into(),
            glyph_thread_scope_join_all as *const () as usize as u64,
        ),
    ])
}

#[test]
fn scoped_borrowed_children_join_and_drain_in_jit() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let temp = tempfile::TempDir::new().unwrap();
    let marker = temp.path().join("jit-drained.txt");
    let output = compile(&source_with_marker(&marker));
    let mut codegen = CodegenContext::new("scoped_thread_source_jit").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    let ir = codegen.dump_ir();
    for runtime_call in [
        "@glyph_thread_scope_create",
        "@glyph_thread_scope_spawn_result",
        "@glyph_thread_scope_join_result",
        "@glyph_thread_scope_drain",
        "@glyph_thread_scope_join_all",
    ] {
        assert!(ir.contains(runtime_call), "missing {runtime_call}\n{ir}");
    }
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "drained");
}

#[test]
fn scoped_borrowed_children_join_and_drain_in_a_native_object() {
    let _guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let temp = tempfile::TempDir::new().unwrap();
    let marker = temp.path().join("native-drained.txt");
    let output = compile(&source_with_marker(&marker));
    let object = temp.path().join("scoped_thread.o");
    let executable = temp.path().join("scoped_thread");
    let mut codegen = CodegenContext::new("scoped_thread_source_object").unwrap();
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
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "drained");
}
