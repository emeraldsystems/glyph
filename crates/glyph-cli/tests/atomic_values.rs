#![cfg(feature = "codegen")]

use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
use glyph_frontend::{FrontendOptions, compile_source};

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    )
}

fn compile_and_run(source: &str) -> i32 {
    let output = compile(source);
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mut codegen = CodegenContext::new("atomic_source_test").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    codegen.jit_execute_i32("main").unwrap()
}

#[test]
fn public_integer_operations_execute_with_observed_value_cas_semantics() {
    let source = r#"
fn main() -> i32 {
  let counter = AtomicI32::new(1)
  let before_add = counter.fetch_add(4)
  let before_swap = counter.swap(9)
  let failed = counter.compare_exchange(7, 11)
  let succeeded = counter.compare_exchange(9, 20)
  let current = counter.load()
  ret before_add + before_swap + failed + succeeded + current
}
"#;

    assert_eq!(compile_and_run(source), 44);
    let output = compile(source);
    let mut codegen = CodegenContext::new("atomic_source_ir").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    let ir = codegen.dump_ir();
    assert!(ir.contains("atomicrmw add"), "{ir}");
    assert!(ir.contains("atomicrmw xchg"), "{ir}");
    assert!(ir.contains("cmpxchg"), "{ir}");
    assert!(ir.contains("seq_cst"), "{ir}");
}

#[test]
fn atomic_bool_swap_store_load_and_compare_exchange_execute() {
    let source = r#"
fn main() -> i32 {
  let flag = AtomicBool::new(false)
  let initial = flag.swap(true)
  if initial { ret 1 }
  let failed = flag.compare_exchange(false, false)
  if !failed { ret 2 }
  flag.store(false)
  if flag.load() { ret 3 }
  ret 42
}
"#;

    assert_eq!(compile_and_run(source), 42);
}

#[test]
fn every_public_atomic_scalar_wrapper_reaches_backend_codegen() {
    let source = r#"
fn main() -> i32 {
  let signed32 = AtomicI32::new(1)
  let unsigned32 = AtomicU32::new(2)
  let signed64 = AtomicI64::new(3)
  let unsigned64 = AtomicU64::new(4)
  let size = AtomicUsize::new(5)
  let flag = AtomicBool::new(true)
  signed32.fetch_sub(1)
  unsigned32.fetch_add(1)
  signed64.swap(6)
  unsigned64.store(7)
  size.load()
  flag.is_lock_free()
  ret signed32.load()
}
"#;

    assert_eq!(compile_and_run(source), 0);
}

#[test]
fn atomic_wrapper_moves_across_function_boundaries_without_plain_storage_reads() {
    let source = r#"
fn transfer(value: AtomicI32) -> AtomicI32 {
  ret value
}

fn main() -> i32 {
  let original = AtomicI32::new(42)
  let transferred = transfer(original)
  ret transferred.load()
}
"#;

    let output = compile(source);
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );
    let mut codegen = CodegenContext::new("atomic_transfer_test").unwrap();
    codegen.codegen_module(&output.mir).unwrap();
    let ir = codegen.dump_ir();
    assert_eq!(compile_and_run(source), 42);
    assert!(
        ir.matches("load atomic i32").count() >= 2,
        "parameter return and explicit load must remain atomic\n{ir}"
    );
}

#[test]
fn atomic_source_emits_links_and_executes_as_a_native_object() {
    let source = r#"
fn main() -> i32 {
  let counter = AtomicI32::new(40)
  counter.fetch_add(2)
  ret counter.load()
}
"#;
    let output = compile(source);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);

    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("atomic.o");
    let executable = temp.path().join("atomic");
    let mut codegen = CodegenContext::new("atomic_object_test").unwrap();
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
fn atomic_wrappers_are_move_only_and_public_orderings_are_not_exposed() {
    let moved = compile(
        r#"
fn main() -> usize {
  let counter = AtomicUsize::new(1)
  let transferred = counter
  transferred.load()
  ret counter.load()
}
"#,
    );
    assert!(
        moved
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("use of moved value `counter`")),
        "diagnostics: {:?}",
        moved.diagnostics
    );

    let ordering_argument = compile(
        r#"
fn main() -> usize {
  let counter = AtomicUsize::new(1)
  ret counter.load(0)
}
"#,
    );
    assert!(
        ordering_argument.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("atomic .load() expects 0 arguments but got 1")
        }),
        "diagnostics: {:?}",
        ordering_argument.diagnostics
    );
}
