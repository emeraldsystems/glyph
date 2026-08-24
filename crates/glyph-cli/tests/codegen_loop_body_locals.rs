//! GLYPH-68 regression: `let` bindings inside a loop body must not leak
//! stack.
//!
//! Only an alloca in a function's ENTRY block is a static frame slot in
//! LLVM. An alloca emitted anywhere else is a dynamic stack allocation
//! that is not reclaimed until the function returns, so a `let` inside a
//! loop body used to move the stack pointer down once per iteration and
//! never move it back. A long-running loop walked into the guard page and
//! the process died with SIGBUS.
//!
//! Two tests, because they fail in different ways and one of them is
//! cheap:
//!
//! * the structural test reads the emitted IR and insists every alloca in
//!   every function sits in that function's entry block. It needs no
//!   target, runs in milliseconds, and names the offending function.
//! * the runtime test builds and runs the fixture. It is the one that
//!   reproduces the original symptom, and it is deliberately sized so a
//!   leak of even a few bytes per iteration cannot survive it.
//!
//! This coverage used to arrive incidentally, via the sequencer example
//! crashing. The sequencer has since moved to the glyph_audio repo, so
//! this fixture inherits the job — and does it better, since it points at
//! the mechanism rather than at a symptom three layers away.

#![cfg(feature = "codegen")]

use std::fs;
use std::path::Path;

use glyph_backend::llvm::LlvmBackend;
use glyph_backend::{Backend, CodegenOptions, EmitKind};
use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(unix)]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
#[cfg(unix)]
use std::{os::unix::process::ExitStatusExt, process::Command};
#[cfg(unix)]
use tempfile::TempDir;

const FIXTURE: &str = "loop_body_locals.glyph";

fn load_fixture(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codegen");
    let path = root.join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture read {}: {}", path.display(), e))
}

/// True for a line that opens a basic block, e.g. `bb3:` or `12:`.
fn is_block_label(line: &str) -> bool {
    let label = match line.split_once(':') {
        Some((label, _)) => label,
        None => return false,
    };
    !label.is_empty()
        && !label.starts_with(' ')
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '-' | '%'))
}

/// Every alloca that is not in its function's entry block.
///
/// Returns `(function, line)` pairs so a failure says where to look
/// instead of just that something is wrong.
///
/// The entry block may or may not carry a label — the backend currently
/// emits `bb0:`, but textual IR is free to leave the first block
/// anonymous. So a label ends the entry block only if instructions have
/// already been seen; a label that comes first is the entry block's own.
fn allocas_outside_entry_block(ir: &str) -> Vec<(String, String)> {
    let mut offenders = Vec::new();
    let mut function: Option<String> = None;
    let mut in_entry_block = false;
    let mut saw_instruction = false;

    for raw in ir.lines() {
        let line = raw.trim();

        if line.starts_with("define ") {
            function = Some(
                line.split_once('@')
                    .map(|(_, rest)| rest.split('(').next().unwrap_or(rest).to_string())
                    .unwrap_or_else(|| line.to_string()),
            );
            in_entry_block = true;
            saw_instruction = false;
            continue;
        }
        if line == "}" {
            function = None;
            continue;
        }

        let Some(name) = function.as_ref() else {
            continue;
        };
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        if is_block_label(line) {
            if saw_instruction {
                in_entry_block = false;
            }
            continue;
        }

        saw_instruction = true;
        if !in_entry_block && line.contains(" = alloca ") {
            offenders.push((name.clone(), line.to_string()));
        }
    }

    offenders
}

/// The detector is the part that can silently stop working: if it never
/// reports anything, the regression test passes forever and protects
/// nothing. So it is checked against IR with a known answer — one alloca
/// correctly in the entry block, one in a loop body where it must be
/// caught — in both the labelled and anonymous entry-block forms.
#[test]
fn the_detector_finds_an_alloca_outside_the_entry_block() {
    let labelled_entry = r#"
define double @good(i32 %0) {
bb0:
  %x = alloca double, align 8
  br label %bb1
bb1:
  ret double 0.0
}
define double @bad(i32 %0) {
bb0:
  %x = alloca double, align 8
  br label %bb1
bb1:
  %leaked = alloca %Chunk, align 8
  ret double 0.0
}
"#;
    let offenders = allocas_outside_entry_block(labelled_entry);
    assert_eq!(
        offenders.len(),
        1,
        "expected exactly the one leaked alloca, got {offenders:?}"
    );
    assert_eq!(offenders[0].0, "bad");
    assert!(offenders[0].1.contains("%leaked"));

    let anonymous_entry = r#"
define double @bad2(i32 %0) {
  %x = alloca double, align 8
  br label %bb1
bb1:
  %leaked = alloca %Chunk, align 8
  ret double 0.0
}
"#;
    let offenders = allocas_outside_entry_block(anonymous_entry);
    assert_eq!(
        offenders.len(),
        1,
        "an anonymous entry block must still be recognised, got {offenders:?}"
    );
    assert_eq!(offenders[0].0, "bad2");
}

#[test]
fn loop_body_locals_are_hoisted_to_the_entry_block() {
    let source = load_fixture(FIXTURE);
    let output = compile_source(
        &source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "fixture failed to compile: {:?}",
        output.diagnostics
    );

    let backend = LlvmBackend::default();
    let artifact = backend
        .emit(
            &output.mir,
            &CodegenOptions {
                emit: EmitKind::LlvmIr,
                ..Default::default()
            },
        )
        .expect("backend emit");
    let ir = artifact.llvm_ir.expect("llvm ir");

    // Sanity: if the fixture stopped containing loops with bindings in
    // them, this test would pass vacuously and protect nothing.
    assert!(
        ir.contains(" = alloca "),
        "fixture emitted no allocas at all — it is no longer exercising anything"
    );

    if std::env::var("GLYPH_DEBUG_IR").is_ok() {
        eprintln!("{ir}");
    }
    let offenders = allocas_outside_entry_block(&ir);
    assert!(
        offenders.is_empty(),
        "{} alloca(s) emitted outside the entry block; in a loop body each one \
         leaks a stack slot per iteration (GLYPH-68):\n{}",
        offenders.len(),
        offenders
            .iter()
            .map(|(f, l)| format!("  in @{f}: {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The original symptom: 100_000 iterations binding ~200 bytes each, on a
/// spawned thread whose stack is a fraction of the main thread's. If the
/// bindings leak, this dies by SIGBUS rather than failing an assertion —
/// which is exactly why the exit status is checked for a signal.
#[cfg(unix)]
#[test]
fn a_long_loop_with_body_locals_does_not_exhaust_the_stack() {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }

    let source = load_fixture(FIXTURE);
    let output = compile_source(
        &source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "fixture failed to compile: {:?}",
        output.diagnostics
    );

    let temp = TempDir::new().unwrap();
    let object = temp.path().join("loop_body_locals.o");
    let executable = temp.path().join("loop_body_locals");

    let mut context = CodegenContext::new("loop_body_locals").unwrap();
    context.codegen_module(&output.mir).unwrap();
    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return;
    }
    context.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object],
            link_libs: Vec::new(),
            link_search_paths: Vec::new(),
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();

    let status = Command::new(&executable)
        .current_dir(temp.path())
        .status()
        .unwrap();

    if let Some(signal) = status.signal() {
        panic!(
            "killed by signal {signal} — a stack-exhaustion crash is the \
             GLYPH-68 symptom; check that allocas are still hoisted to the \
             entry block"
        );
    }
    assert_eq!(
        status.code(),
        Some(0),
        "fixture exited nonzero; see its `ret` codes for which stage failed"
    );
}
