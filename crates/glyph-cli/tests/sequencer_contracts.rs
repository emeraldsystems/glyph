//! GLYPH-54 acceptance: the sequencer-core contract vocabulary from
//! docs/design/SEQUENCER_CORE.md compiles and round-trips through real
//! channels and a real thread. The fixture sends every EngineCommand
//! variant (including a streamed PatternBegin/Track/Note/Commit pattern)
//! into a spawned engine loop, which assembles the pattern in thread-local
//! Vecs and answers with every EngineReport variant.
//!
//! If this test and the ADR disagree, this fixture wins and the ADR must
//! be updated.

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use std::os::unix::process::ExitStatusExt;

#[cfg(all(feature = "codegen", unix))]
use std::process::Command;

#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
#[test]
fn sequencer_contract_vocabulary_round_trips() {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sequencer/contracts.glyph");
    let source = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("fixture read {}: {}", fixture.display(), e));

    let frontend_output = compile_source(
        &source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        frontend_output.diagnostics.is_empty(),
        "contract fixture failed to compile: {:?}",
        frontend_output.diagnostics
    );

    let temp = TempDir::new().unwrap();
    let obj_path = temp.path().join("contracts.o");
    let exe_path = temp.path().join("contracts");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return;
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
    let code = status
        .code()
        .or_else(|| status.signal().map(|s| -s))
        .unwrap_or(-1);
    assert_eq!(
        code, 0,
        "contract round-trip exited {}; see fixture exit-code map",
        code
    );
}
