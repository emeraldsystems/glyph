#![cfg(all(feature = "codegen", unix))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use glyph_backend::codegen::CodegenContext;
use glyph_frontend::{FrontendOptions, compile_source};

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);
type ThreadResultEntry = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<ThreadEntry>,
    ) -> i32;
    fn glyph_thread_spawn_result(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        drop_unstarted: Option<ThreadEntry>,
        result_size: usize,
        drop_result: Option<ThreadEntry>,
    ) -> i32;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_join_result(handle: *mut *mut GlyphThread, out_result: *mut c_void) -> i32;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> i32;
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

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
            "glyph_thread_spawn_result".into(),
            glyph_thread_spawn_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_join".into(),
            glyph_thread_join as *const () as usize as u64,
        ),
        (
            "glyph_thread_join_result".into(),
            glyph_thread_join_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_detach".into(),
            glyph_thread_detach as *const () as usize as u64,
        ),
    ])
}

fn execute(source: &str, module_name: &str) -> i32 {
    let output = compile(source);
    let mut codegen = CodegenContext::new(module_name).unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen
        .jit_execute_i32_with_symbols("main", &runtime_symbols())
        .unwrap()
}

#[test]
fn source_spawn_and_join_specialize_an_i32_result() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn answer() -> i32 { ret 42 }

fn main() -> i32 {
  let spawned = spawn(answer)
  ret match spawned {
    Ok(handle) => match handle.join() {
      Ok(value) => value,
      Err(_error) => 2,
    },
    Err(_error) => 1,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_i32"), 42);
}

#[test]
fn source_spawn_infers_an_inline_closure_result_without_generic_functions() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn main() -> i32 {
  let spawned = spawn(move () -> { 42 })
  ret match spawned {
    Ok(handle) => match handle.join() {
      Ok(value) => value,
      Err(_error) => 2,
    },
    Err(_error) => 1,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_closure"), 42);
}

#[test]
fn source_spawn_and_join_preserve_a_large_sret_struct() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

struct BigResult {
  f0: i32,
  f1: i32,
  f2: i32,
  f3: i32,
  f4: i32,
  f5: i32,
  f6: i32,
  f7: i32,
}

fn build_result() -> BigResult {
  ret BigResult {
    f0: 40,
    f1: 41,
    f2: 42,
    f3: 43,
    f4: 44,
    f5: 45,
    f6: 46,
    f7: 47,
  }
}

fn main() -> i32 {
  let spawned = spawn(build_result)
  ret match spawned {
    Ok(handle) => match handle.join() {
      Ok(value) => value.f7,
      Err(_error) => 2,
    },
    Err(_error) => 1,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_sret"), 47);
}

#[test]
fn source_join_transfers_an_owned_string_result() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn build_message() -> String {
  ret String::from_str("owned thread result")
}

fn main() -> i32 {
  let spawned = spawn(build_message)
  ret match spawned {
    Ok(handle) => match handle.join() {
      Ok(message) => {
        let length = message.len()
        42
      },
      Err(_error) => 2,
    },
    Err(_error) => 1,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_string"), 42);
}

#[test]
fn source_join_preserves_tuple_and_enum_result_payloads() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

enum Signal {
  Value(i32)
  Empty
}

fn build_pair() -> (i32, i32) { ret (10, 9) }
fn build_signal() -> Signal { ret Signal::Value(23) }

fn main() -> i32 {
  let pair_spawned = spawn(build_pair)
  let pair_total = match pair_spawned {
    Ok(handle) => match handle.join() {
      Ok(pair) => pair.0 + pair.1,
      Err(_error) => 100,
    },
    Err(_error) => 100,
  }
  let signal_spawned = spawn(build_signal)
  ret match signal_spawned {
    Ok(handle) => match handle.join() {
      Ok(signal) => match signal {
        Value(value) => pair_total + value,
        Empty => 101,
      },
      Err(_error) => 102,
    },
    Err(_error) => 103,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_tuple_enum"), 42);
}

#[test]
fn source_typed_child_can_return_early() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn choose_answer() -> i32 {
  if true { ret 42 }
  ret 0
}

fn main() -> i32 {
  let spawned = spawn(choose_answer)
  ret match spawned {
    Ok(handle) => match handle.join() {
      Ok(value) => value,
      Err(_error) => 2,
    },
    Err(_error) => 1,
  }
}
"#;

    assert_eq!(execute(source, "thread_source_typed_early_return"), 42);
}

#[test]
fn source_dropping_a_typed_handle_detaches_it() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn build_message() -> String {
  ret String::from_str("unclaimed thread result")
}

fn main() -> i32 {
  let spawned = spawn(build_message)
  match spawned {
    Ok(handle) => {},
    Err(_error) => { ret 1 },
  }
  ret 42
}
"#;

    assert_eq!(execute(source, "thread_source_typed_handle_drop"), 42);
}

#[test]
fn source_typed_spawn_join_stress_preserves_every_result() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let source = r#"
import spawn from std/thread

fn answer() -> i32 { ret 42 }

fn main() -> i32 {
  let mut total: i32 = 0
  let mut i: i32 = 0
  while i < 64 {
    let spawned = spawn(answer)
    match spawned {
      Ok(handle) => match handle.join() {
        Ok(value) => { total = total + value },
        Err(_error) => { ret 2 },
      },
      Err(_error) => { ret 1 },
    }
    i = i + 1
  }
  ret total
}
"#;

    assert_eq!(execute(source, "thread_source_typed_stress"), 64 * 42);
}
