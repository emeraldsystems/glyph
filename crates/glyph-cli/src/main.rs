use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use glyph_backend::{Backend, CodegenOptions, EmitKind};
use glyph_frontend::{
    FrontendOptions, ResolverContext, compile_modules, resolve_multi_module, resolve_types,
};

mod module_loader;
use module_loader::{discover_and_parse_modules, module_id_from_path};

mod diagnostics;
use diagnostics::format_diagnostic;

mod build_version;

#[cfg(feature = "codegen")]
mod thread_runtime;

#[cfg(not(feature = "codegen"))]
use glyph_backend::NullBackend;
#[cfg(feature = "codegen")]
use glyph_backend::llvm::LlvmBackend;
#[cfg(feature = "codegen")]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[derive(Parser, Debug)]
#[command(
    name = "glyph",
    version = crate::build_version::BUILD_VERSION,
    about = "Glyph language toolchain"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Parse and type-check source
    Check { path: PathBuf },
    /// Build source and emit code
    Build {
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = EmitTarget::Ll)]
        emit: EmitTarget,
        /// Link against a library (can be specified multiple times)
        #[arg(long = "link-lib")]
        link_lib: Vec<String>,
        /// Add a library search path (can be specified multiple times)
        #[arg(long = "link-search")]
        link_search: Vec<PathBuf>,
    },
    /// Build and execute (stub)
    Run { path: PathBuf },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum EmitTarget {
    Ll,
    Obj,
    Exe,
}

impl From<EmitTarget> for EmitKind {
    fn from(value: EmitTarget) -> Self {
        match value {
            EmitTarget::Ll => EmitKind::LlvmIr,
            EmitTarget::Obj => EmitKind::Object,
            EmitTarget::Exe => EmitKind::Executable,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check { path } => check(&path),
        Commands::Build {
            path,
            emit,
            link_lib,
            link_search,
        } => build(&path, emit, link_lib, link_search),
        Commands::Run { path } => run(&path),
    }
}

fn check(path: &PathBuf) -> Result<()> {
    // Determine project root (parent of the input file)
    let project_root = path.parent().unwrap_or(Path::new("."));
    let entry_module = module_id_from_path(project_root, path)?;

    // Discover and parse all .glyph files in the project
    let load = discover_and_parse_modules(project_root)?;
    if !load.diagnostics.is_empty() {
        for diag in &load.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &load.sources));
        }
        return Err(anyhow!("module load failed"));
    }
    let modules = load.modules;
    let sources = load.sources;

    if modules.is_empty() {
        return Err(anyhow!("no .glyph files found in project"));
    }

    // Multi-module resolution
    let (multi_ctx, compile_order) = resolve_multi_module(modules, &entry_module, project_root)
        .map_err(|diags| {
            for diag in &diags {
                eprintln!("{}", format_diagnostic(diag, &sources));
            }
            anyhow!("multi-module resolution failed")
        })?;

    // Type-check each module in dependency order
    let mut all_diagnostics = Vec::new();

    for module_id in &compile_order {
        let module = &multi_ctx.modules[module_id];
        let import_scope = &multi_ctx.import_scopes[module_id];

        // Create resolver context with module info
        let mut resolver_ctx = ResolverContext::default();
        resolver_ctx.current_module = Some(module_id.clone());
        resolver_ctx.import_scope = Some(import_scope.clone());
        resolver_ctx.all_modules = Some(multi_ctx.clone());

        // Resolve types for this module
        let (_, diags) = resolve_types(module);
        all_diagnostics.extend(diags.into_iter().map(|diag| {
            if diag.module_id.is_some() {
                diag
            } else {
                diag.with_module_id(module_id.clone())
            }
        }));
    }

    // Report results
    if all_diagnostics.is_empty() {
        println!("check ok: {} module(s), 0 diagnostics", compile_order.len());
        Ok(())
    } else {
        for diag in &all_diagnostics {
            eprintln!("{}", format_diagnostic(diag, &sources));
        }
        Err(anyhow!(
            "check failed with {} diagnostic(s)",
            all_diagnostics.len()
        ))
    }
}

fn build(
    path: &PathBuf,
    emit: EmitTarget,
    link_lib: Vec<String>,
    link_search: Vec<PathBuf>,
) -> Result<()> {
    let project_root = path.parent().unwrap_or(Path::new("."));
    let entry_module = module_id_from_path(project_root, path)?;
    let load = discover_and_parse_modules(project_root)?;
    if !load.diagnostics.is_empty() {
        for diag in &load.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &load.sources));
        }
        return Err(anyhow!("module load failed"));
    }
    let modules = load.modules;
    let sources = load.sources;
    let output = compile_modules(
        modules,
        &entry_module,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    if !output.diagnostics.is_empty() {
        for diag in &output.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &sources));
        }
        return Err(anyhow!("build failed"));
    }

    // Determine output file names based on input path
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Invalid input file path"))?;

    match emit {
        EmitTarget::Ll => {
            // Just emit LLVM IR to stdout (existing behavior)
            #[cfg(feature = "codegen")]
            {
                let backend = LlvmBackend::default();
                let opts = CodegenOptions {
                    emit: EmitKind::LlvmIr,
                    link_libs: link_lib,
                    link_search_paths: link_search,
                    ..Default::default()
                };
                let artifact = backend.emit(&output.mir, &opts)?;
                if let Some(ir) = artifact.llvm_ir {
                    println!("{}", ir);
                }
            }
            #[cfg(not(feature = "codegen"))]
            {
                let backend = NullBackend::default();
                let opts = CodegenOptions::default();
                let artifact = backend.emit(&output.mir, &opts)?;
                if let Some(ir) = artifact.llvm_ir {
                    println!("{}", ir);
                }
            }
        }
        EmitTarget::Obj => {
            // Generate object file
            #[cfg(feature = "codegen")]
            {
                let obj_path = PathBuf::from(format!("{}.o", stem));
                let mut ctx = CodegenContext::new("glyph_module")?;
                ctx.codegen_module(&output.mir)?;
                ctx.emit_object_file(&obj_path)?;
                println!("Object file written to: {}", obj_path.display());
            }
            #[cfg(not(feature = "codegen"))]
            {
                return Err(anyhow!(
                    "Object file emission requires the 'codegen' feature"
                ));
            }
        }
        EmitTarget::Exe => {
            // Generate object file, then link to executable
            #[cfg(feature = "codegen")]
            {
                let obj_path = PathBuf::from(format!("{}.o", stem));
                let exe_path = PathBuf::from(stem);

                // Generate object file using LLVM
                let mut ctx = CodegenContext::new("glyph_module")?;
                ctx.codegen_module(&output.mir)?;
                ctx.emit_object_file(&obj_path)?;

                // Link to executable
                let linker = Linker::new();
                let runtime_lib = Linker::get_runtime_lib_path();

                let linker_opts = LinkerOptions {
                    output_path: exe_path.clone(),
                    object_files: vec![obj_path.clone()],
                    link_libs: link_lib,
                    link_search_paths: link_search,
                    runtime_lib_path: runtime_lib,
                };

                linker.link(&linker_opts)?;

                // Clean up intermediate object file
                let _ = std::fs::remove_file(obj_path);

                println!("Executable written to: {}", exe_path.display());
            }
            #[cfg(not(feature = "codegen"))]
            {
                return Err(anyhow!(
                    "Executable generation requires the 'codegen' feature"
                ));
            }
        }
    }

    Ok(())
}

fn run(path: &PathBuf) -> Result<()> {
    let project_root = path.parent().unwrap_or(Path::new("."));
    let entry_module = module_id_from_path(project_root, path)?;
    let load = discover_and_parse_modules(project_root)?;
    if !load.diagnostics.is_empty() {
        for diag in &load.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &load.sources));
        }
        return Err(anyhow!("module load failed"));
    }
    let modules = load.modules;
    let sources = load.sources;
    let output = compile_modules(
        modules,
        &entry_module,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    if !output.diagnostics.is_empty() {
        for diag in &output.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &sources));
        }
        return Err(anyhow!("build failed"));
    }

    #[cfg(feature = "codegen")]
    {
        // Prefer JIT execution for `run` to avoid platform-specific AOT linker/toolchain
        // issues and to keep the feedback loop fast.
        let mut ctx = CodegenContext::new("glyph_module")?;
        ctx.codegen_module(&output.mir)?;

        // Runtime symbols that are normally supplied by the AOT runtime
        // library. This map is now just an *optional override* point: every
        // runtime/*.c function (glyph_fmt_*, glyph_json_*, glyph_time_*,
        // glyph_term_*, glyph_process_run, glyph_net_*, glyph_audio_*, ...)
        // is force-loaded into this binary's process image (see
        // build.rs::force_load_runtime_archive) and gets resolved
        // automatically by `jit_execute_i32_with_symbols` via a
        // process-wide symbol search for anything not listed here
        // (glyph-backend/src/codegen/emit.rs, GLYPH-83). The thread runtime
        // symbols are still registered explicitly because
        // `thread_runtime::register_symbols`'s doc comment notes a second
        // reason for referencing them directly from Rust: doing so is what
        // keeps those particular archive members retained in the first
        // place for other (non-force-loaded) build configurations.
        let mut symbols = HashMap::new();
        thread_runtime::register_symbols(&mut symbols);

        let exit = ctx.jit_execute_i32_with_symbols("main", &symbols)?;
        if exit != 0 {
            return Err(anyhow!("Program exited with status code: {}", exit));
        }
        Ok(())
    }

    #[cfg(not(feature = "codegen"))]
    {
        let _ = output;
        Err(anyhow!("running programs requires the 'codegen' feature"))
    }
}

// GLYPH-83: this file used to carry hand-written Rust re-implementations of
// glyph_byte_at, glyph_time_*, glyph_process_run, and glyph_term_* here so
// the JIT path in `run()` above could register their addresses (the real
// runtime/*.c definitions of those same functions were never linked into
// this binary, because nothing in Rust referenced them, so the static
// archive dropped those .o members). That is no longer true: build.rs now
// force-loads the entire runtime archive into this crate's binaries (see
// `force_load_runtime_archive` there), so every runtime/*.c function is
// present in-process, and `CodegenContext::jit_execute_i32_with_symbols`
// resolves any extern not in its explicit `symbols` map by searching the
// process image directly (glyph-backend/src/codegen/emit.rs). Keeping both
// the Rust duplicates and the force-loaded C originals would double-define
// the same symbol names and fail to link, so the duplicates were deleted;
// the real runtime/*.c implementations (already exercised by the AOT path)
// are now the single implementation for both JIT and AOT execution.

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_help_succeeds() {
        let err = Cli::try_parse_from(["glyph", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn emit_target_maps_to_emit_kind() {
        assert!(matches!(EmitKind::from(EmitTarget::Ll), EmitKind::LlvmIr));
        assert!(matches!(EmitKind::from(EmitTarget::Obj), EmitKind::Object));
        assert!(matches!(
            EmitKind::from(EmitTarget::Exe),
            EmitKind::Executable
        ));
    }

    #[test]
    fn discover_modules_finds_all_files() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create test files
        fs::write(root.join("main.glyph"), "fn main() -> i32 { ret 0 }").unwrap();
        fs::write(root.join("utils.glyph"), "fn helper() -> i32 { ret 1 }").unwrap();

        let math_dir = root.join("math");
        fs::create_dir(&math_dir).unwrap();
        fs::write(math_dir.join("geometry.glyph"), "struct Point { x: i32 }").unwrap();

        // Discover modules
        let load = discover_and_parse_modules(root).unwrap();
        let modules = load.modules;

        // Verify we found all 3 files
        assert_eq!(modules.len(), 3);
        assert!(modules.contains_key("main"));
        assert!(modules.contains_key("utils"));
        assert!(modules.contains_key("math/geometry"));
    }

    #[cfg(all(feature = "codegen", unix))]
    #[test]
    fn jit_time_format_buffer_is_thread_local() {
        use std::ffi::CStr;
        use std::ffi::c_char;
        use std::sync::{Arc, Barrier};

        // GLYPH-83: glyph_time_to_human_readable no longer has a Rust
        // duplicate in this crate (see the comment above `mod tests`) — it
        // is provided by the force-loaded runtime/glyph_time.c archive
        // member, exactly as the AOT path already used. Declare it here to
        // exercise the real, shared implementation's thread-local scratch
        // buffer directly, the same way thread_runtime.rs's tests declare
        // the thread/mutex runtime symbols they exercise.
        unsafe extern "C" {
            fn glyph_time_to_human_readable(ts: u64) -> *const c_char;
        }

        let first_formatted = Arc::new(Barrier::new(2));
        let second_formatted = Arc::new(Barrier::new(2));

        let first_ready = Arc::clone(&first_formatted);
        let second_ready = Arc::clone(&second_formatted);
        let first = std::thread::spawn(move || {
            let view = unsafe { glyph_time_to_human_readable(0) };
            first_ready.wait();
            second_ready.wait();
            unsafe { CStr::from_ptr(view) }
                .to_string_lossy()
                .into_owned()
        });

        let first_ready = Arc::clone(&first_formatted);
        let second_ready = Arc::clone(&second_formatted);
        let second = std::thread::spawn(move || {
            first_ready.wait();
            let view = unsafe { glyph_time_to_human_readable(86_400) };
            let value = unsafe { CStr::from_ptr(view) }
                .to_string_lossy()
                .into_owned();
            second_ready.wait();
            value
        });

        assert_eq!(first.join().unwrap(), "01/01/1970 00:00:00");
        assert_eq!(second.join().unwrap(), "02/01/1970 00:00:00");
    }
}
