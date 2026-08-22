#![cfg(all(feature = "codegen", unix))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use glyph_backend::codegen::CodegenContext;
use glyph_backend::linker::{Linker, LinkerOptions};
use glyph_frontend::{FrontendOptions, compile_source};

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

#[repr(C)]
struct GlyphMutex {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<ThreadEntry>,
    ) -> i32;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_mutex_create(out: *mut *mut GlyphMutex) -> i32;
    fn glyph_mutex_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_try_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_unlock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_destroy(mutex: *mut *mut GlyphMutex) -> i32;
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
        (
            "glyph_mutex_create".into(),
            glyph_mutex_create as *const () as usize as u64,
        ),
        (
            "glyph_mutex_lock".into(),
            glyph_mutex_lock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_try_lock".into(),
            glyph_mutex_try_lock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_unlock".into(),
            glyph_mutex_unlock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_destroy".into(),
            glyph_mutex_destroy as *const () as usize as u64,
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

const BASIC_MUTEX_SOURCE: &str = r#"
import Mutex from std/sync

fn main() -> i32 {
  let state = Mutex::new(AtomicI32::new(41))
  {
    let guard = state.lock()
    let value = guard.borrow_mut()
    let previous = value.fetch_add(1)
  }
  ret match state.try_lock() {
    Some(guard) => {
      let value = guard.borrow()
      value.load()
    },
    None => 0,
  }
}
"#;

#[test]
fn canonical_mutex_executes_lock_try_lock_and_guard_drop_in_jit() {
    let output = compile(BASIC_MUTEX_SOURCE);
    let mut codegen = CodegenContext::new("mutex_source_jit").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    let ir = codegen.dump_ir();
    assert!(ir.contains("call i32 @glyph_mutex_lock"), "{ir}");
    assert!(ir.contains("call i32 @glyph_mutex_try_lock"), "{ir}");
    assert!(ir.contains("call i32 @glyph_mutex_unlock"), "{ir}");
}

#[test]
fn canonical_mutex_source_links_and_runs_as_a_native_object() {
    let output = compile(BASIC_MUTEX_SOURCE);
    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("mutex.o");
    let executable = temp.path().join("mutex");
    let mut codegen = CodegenContext::new("mutex_source_object").unwrap();
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

const ARC_MUTEX_STRESS: &str = r#"
import Arc from std/sync
import Mutex from std/sync
import spawn from std/thread

fn main() -> i32 {
  let state = Arc::new(Mutex::new(AtomicI32::new(0)))
  let c1 = state.clone()
  let c2 = state.clone()
  let c3 = state.clone()
  let c4 = state.clone()
  let t1: FnOnce<(), ()> = move () -> {
    let mutex = c1.borrow()
    for i in 0..1000 {
      let guard = mutex.lock()
      let value = guard.borrow_mut()
      let previous = value.fetch_add(1)
    }
    let done = 0
  }
  let t2: FnOnce<(), ()> = move () -> {
    let mutex = c2.borrow()
    for i in 0..1000 {
      let guard = mutex.lock()
      let value = guard.borrow_mut()
      let previous = value.fetch_add(1)
    }
    let done = 0
  }
  let t3: FnOnce<(), ()> = move () -> {
    let mutex = c3.borrow()
    for i in 0..1000 {
      let guard = mutex.lock()
      let value = guard.borrow_mut()
      let previous = value.fetch_add(1)
    }
    let done = 0
  }
  let t4: FnOnce<(), ()> = move () -> {
    let mutex = c4.borrow()
    for i in 0..1000 {
      let guard = mutex.lock()
      let value = guard.borrow_mut()
      let previous = value.fetch_add(1)
    }
    let done = 0
  }
  let h1 = spawn(t1)
  let h2 = spawn(t2)
  let h3 = spawn(t3)
  let h4 = spawn(t4)
  match h1 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  match h2 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  match h3 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  match h4 { Ok(handle) => { let joined = handle.join() }, Err(_error) => {} }
  let mutex = state.borrow()
  let guard = mutex.lock()
  let value = guard.borrow()
  ret value.load()
}
"#;

#[test]
fn arc_mutex_protects_a_source_counter_under_worker_contention() {
    let output = compile(ARC_MUTEX_STRESS);
    let mut codegen = CodegenContext::new("arc_mutex_source_stress").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        4_000
    );
}

static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn tracked_free(pointer: *mut c_void) {
    FREE_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
}

#[test]
fn source_mutex_drops_owned_payload_exactly_once() {
    let output = compile(
        r#"
import Mutex from std/sync
fn main() -> i32 {
  let state = Mutex::new(Own::new(7))
  let guard = state.lock()
  ret 0
}
"#,
    );
    FREE_CALLS.store(0, Ordering::SeqCst);
    let mut codegen = CodegenContext::new("mutex_source_drop").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    let mut symbols = runtime_symbols();
    symbols.insert("free".into(), tracked_free as *const () as usize as u64);
    assert_eq!(
        codegen
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        0
    );
    assert_eq!(
        FREE_CALLS.load(Ordering::SeqCst),
        2,
        "Own payload and compiler Mutex control block must each free once"
    );
}
