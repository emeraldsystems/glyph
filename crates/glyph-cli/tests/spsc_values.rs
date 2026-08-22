#![cfg(all(feature = "codegen", unix))]

use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
use glyph_frontend::{FrontendOptions, compile_source};

const SOURCE: &str = r#"
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult

fn main() -> i32 {
  let mut receiver: Receiver<i32>
  let sender: Sender<i32> = channel(1, &mut receiver)

  let first = sender.try_send(10)
  let sent_code = match first {
    Sent => 1,
    Full(_value) => 100,
    Disconnected(_value) => 101,
  }

  let second = sender.try_send(20)
  let returned = match second {
    Sent => 102,
    Full(value) => value,
    Disconnected(value) => value + 100,
  }

  let received = receiver.try_recv()
  let value = match received {
    Value(item) => item,
    Empty => 103,
    Disconnected => 104,
  }

  let empty = receiver.try_recv()
  let empty_code = match empty {
    Value(_item) => 105,
    Empty => 1,
    Disconnected => 106,
  }
  ret sent_code + returned + value + empty_code
}
"#;

fn compile() -> glyph_frontend::FrontendOutput {
    let output = compile_source(
        SOURCE,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
}

#[test]
fn source_spsc_executes_in_jit_and_keeps_failed_send_ownership() {
    let output = compile();
    let mut context = CodegenContext::new("spsc_source_jit").unwrap();
    context.codegen_module(&output.mir).unwrap();
    assert_eq!(context.jit_execute_i32("main").unwrap(), 32);
    let ir = context.dump_ir();
    assert!(ir.contains("spsc.send.publish"), "{ir}");
    assert!(ir.contains("spsc.recv.value"), "{ir}");
    assert_eq!(ir.matches("call ptr @malloc").count(), 1, "{ir}");
}

#[test]
fn source_spsc_links_and_executes_as_a_native_object() {
    let output = compile();
    let temp = tempfile::TempDir::new().unwrap();
    let object = temp.path().join("spsc.o");
    let executable = temp.path().join("spsc");
    let mut context = CodegenContext::new("spsc_source_object").unwrap();
    context.codegen_module(&output.mir).unwrap();
    context.emit_object_file(&object).unwrap();
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
        Some(32)
    );
}
