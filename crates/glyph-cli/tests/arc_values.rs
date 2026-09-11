#![cfg(all(feature = "codegen", unix))]

use std::collections::HashMap;
use std::ffi::c_void;

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

const ARC_THREAD_SOURCE: &str = r#"
import Arc from std/sync
import spawn from std/thread

fn main() -> i32 {
  let counter = Arc::new(AtomicI32::new(0))
  let worker_counter = counter.clone()
  let task: FnOnce<(), ()> = move () -> {
    let atomic = worker_counter.borrow()
    let previous = atomic.fetch_add(42)
  }
  let outcome = spawn(task)
  ret match outcome {
    Ok(handle) => {
      let joined = handle.join()
      let atomic = counter.borrow()
      atomic.load()
    },
    Err(_error) => 1,
  }
}
"#;

#[test]
fn canonical_arc_crosses_a_source_thread_and_executes_in_jit() {
    let output = compile(ARC_THREAD_SOURCE);
    let mut codegen = CodegenContext::new("arc_source_jit").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    let ir = codegen.dump_ir();
    assert!(ir.contains("atomicrmw add ptr"), "{ir}");
    assert!(ir.contains("atomicrmw sub ptr"), "{ir}");
    assert!(ir.contains("call i32 @glyph_thread_spawn"), "{ir}");
}

#[test]
fn canonical_arc_source_links_and_runs_as_a_native_object() {
    let output = compile(ARC_THREAD_SOURCE);
    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("arc.o");
    let executable = temp.path().join("arc");
    let mut codegen = CodegenContext::new("arc_source_object").unwrap();
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
fn many_source_workers_clone_read_and_drop_the_same_arc() {
    let source = r#"
import Arc from std/sync
import spawn from std/thread

fn main() -> i32 {
  let counter = Arc::new(AtomicI32::new(0))
  let c1 = counter.clone()
  let c2 = counter.clone()
  let c3 = counter.clone()
  let t1: FnOnce<(), ()> = move () -> {
    let a = c1.borrow()
    let previous = a.fetch_add(1)
  }
  let t2: FnOnce<(), ()> = move () -> {
    let a = c2.borrow()
    let previous = a.fetch_add(1)
  }
  let t3: FnOnce<(), ()> = move () -> {
    let a = c3.borrow()
    let previous = a.fetch_add(1)
  }
  let h1 = spawn(t1)
  let h2 = spawn(t2)
  let h3 = spawn(t3)
  match h1 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  match h2 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  match h3 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  let atomic = counter.borrow()
  ret atomic.load()
}
"#;
    let output = compile(source);
    let mut codegen = CodegenContext::new("arc_source_stress").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        3
    );
}
