//! Breadth coverage for the GLYPH-3 MIR verifier (`glyph_core::mir_verify`).
//!
//! The verifier is only as trustworthy as the corpus it has been run over: a
//! false positive would break every program that hits the flagged shape, and
//! the full `cargo test` suite is too large to run in this environment (see
//! AGENTS/worktree notes on disk budget). This test substitutes breadth for
//! that: it frontend-compiles (no LLVM — parse + resolve + lower +
//! monomorphize only, so it is cheap) every `.glyph` program under
//! `examples/**`, `tests/fixtures/**`, and the embedded stdlib module
//! registry (`glyph_frontend::std_modules`), and asserts the verifier finds
//! zero errors in every one that compiles cleanly today.
//!
//! Programs that do not compile cleanly (some fixtures are deliberately
//! invalid, e.g. `tests/fixtures/mir/break_outside_loop.glyph`; some are
//! parser-only snippets with no `main`; `case_demo`'s cross-package path
//! dependency is out of scope for this file's minimal project loader) are
//! skipped and reported rather than asserted on, matching what a verifier
//! breadth test can and cannot promise.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use glyph_core::ast::Module;
use glyph_core::mir_verify::{format_errors, verify_module};
use glyph_frontend::{FrontendOptions, compile_modules, compile_source, std_modules};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn glyph_opts() -> FrontendOptions {
    FrontendOptions {
        emit_mir: true,
        include_std: true,
    }
}

/// Reimplements the minimal slice of `glyph-cli`'s `module_loader` needed
/// here: read every `.glyph` file under `root` and key it by its
/// slash-separated, extension-stripped path relative to `root`. `glyph-cli`
/// is a bin-only crate (no lib target), so its own `module_loader` module is
/// not reachable from an integration test; this is a deliberately small,
/// independent reimplementation rather than a dependency on it.
fn discover_modules(root: &Path) -> Result<HashMap<String, Module>, String> {
    let mut modules = HashMap::new();
    for entry in walkdir::WalkDir::new(root).follow_links(true) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("glyph") {
            continue;
        }
        let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let module_id = module_id_from_path(root, path)?;
        let lex_out = glyph_frontend::lex(&source);
        if !lex_out.diagnostics.is_empty() {
            return Err(format!("{}: {} lex diagnostic(s)", module_id, lex_out.diagnostics.len()));
        }
        let parse_out = glyph_frontend::parse(&lex_out.tokens, &source);
        if !parse_out.diagnostics.is_empty() {
            return Err(format!(
                "{}: {} parse diagnostic(s)",
                module_id,
                parse_out.diagnostics.len()
            ));
        }
        modules.insert(module_id, parse_out.module);
    }
    Ok(modules)
}

fn module_id_from_path(root: &Path, path: &Path) -> Result<String, String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.with_extension("")
        .to_str()
        .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
        .ok_or_else(|| format!("non-utf8 path: {:?}", rel))
}

/// Read `dir/glyph.toml` and return `(project_root, entry_module_id)` for its
/// first `[[bin]]` entry, mirroring how `glyph-cli build`/`check` derive
/// these from a manifest.
fn entry_from_manifest(dir: &Path) -> Option<(PathBuf, String)> {
    let manifest_path = dir.join("glyph.toml");
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let value: toml::Value = content.parse().ok()?;
    let bin_path = value.get("bin")?.as_array()?.first()?.get("path")?.as_str()?;
    let full_path = dir.join(bin_path);
    let project_root = full_path.parent()?.to_path_buf();
    let module_id = module_id_from_path(&project_root, &full_path).ok()?;
    Some((project_root, module_id))
}

#[derive(Default)]
struct CorpusTally {
    verified: usize,
    skipped: Vec<String>,
    failures: Vec<String>,
}

impl CorpusTally {
    fn record_compiled(&mut self, label: &str, output: &glyph_frontend::FrontendOutput) {
        if !output.diagnostics.is_empty() {
            self.skipped.push(format!(
                "{}: {} frontend diagnostic(s), not attempting verification",
                label,
                output.diagnostics.len()
            ));
            return;
        }
        let errors = verify_module(&output.mir);
        if errors.is_empty() {
            self.verified += 1;
        } else {
            self.failures.push(format!(
                "{}: verifier rejected a cleanly-compiled program:\n{}",
                label,
                format_errors(&errors)
            ));
        }
    }
}

fn check_single_source(source: &str, label: &str, tally: &mut CorpusTally) {
    let output = compile_source(source, glyph_opts());
    tally.record_compiled(label, &output);
}

#[test]
fn verifier_accepts_every_cleanly_compiling_corpus_program() {
    let root = workspace_root();
    let mut tally = CorpusTally::default();

    // --- examples/**, grouped by glyph.toml where present -----------------
    let examples_root = root.join("examples");
    let mut toml_dirs: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&examples_root) {
        let entry = entry.expect("walk examples/");
        if entry.file_name() == "glyph.toml" {
            toml_dirs.push(entry.path().parent().unwrap().to_path_buf());
        }
    }

    for dir in &toml_dirs {
        let label = dir
            .strip_prefix(&root)
            .unwrap_or(dir)
            .display()
            .to_string();
        let Some((project_root, entry_module)) = entry_from_manifest(dir) else {
            tally
                .skipped
                .push(format!("{}: could not read glyph.toml [[bin]] entry", label));
            continue;
        };
        let modules = match discover_modules(&project_root) {
            Ok(m) => m,
            Err(e) => {
                tally.skipped.push(format!("{}: {}", label, e));
                continue;
            }
        };
        let output = compile_modules(modules, &entry_module, glyph_opts());
        tally.record_compiled(&label, &output);
    }

    // Loose top-level .glyph files under examples/ that are not part of a
    // glyph.toml project (hello_test/, puts_hello/, std_hello/): each is its
    // own standalone single-file program.
    for entry in walkdir::WalkDir::new(&examples_root) {
        let entry = entry.expect("walk examples/");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("glyph") {
            continue;
        }
        if toml_dirs.iter().any(|d| path.starts_with(d)) {
            continue;
        }
        let label = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                tally.skipped.push(format!("{}: read error: {}", label, e));
                continue;
            }
        };
        check_single_source(&source, &label, &mut tally);
    }

    // --- tests/fixtures/**, each file a standalone single-file program -----
    let fixtures_root = root.join("tests/fixtures");
    for entry in walkdir::WalkDir::new(&fixtures_root) {
        let entry = entry.expect("walk tests/fixtures/");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("glyph") {
            continue;
        }
        let label = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                tally.skipped.push(format!("{}: read error: {}", label, e));
                continue;
            }
        };
        check_single_source(&source, &label, &mut tally);
    }

    // --- crates/glyph-frontend/src/stdlib/**, driven by the registered
    //     module map (not the raw filesystem) so this only ever attempts
    //     modules the compiler actually knows about. ----------------------
    let module_ids: Vec<String> = std_modules().keys().cloned().collect();
    for module_id in module_ids {
        let label = format!("std module `{}`", module_id);
        let output = compile_modules(std_modules(), &module_id, glyph_opts());
        tally.record_compiled(&label, &output);
    }

    eprintln!(
        "mir_verify_corpus: {} program(s) verified clean, {} skipped, {} FAILED",
        tally.verified,
        tally.skipped.len(),
        tally.failures.len()
    );
    let skipped_preview: HashSet<&String> = tally.skipped.iter().collect();
    for s in &skipped_preview {
        eprintln!("  skipped: {}", s);
    }

    assert!(
        tally.failures.is_empty(),
        "the verifier rejected {} cleanly-compiling corpus program(s):\n{}",
        tally.failures.len(),
        tally.failures.join("\n---\n")
    );
    assert!(
        tally.verified > 20,
        "expected a substantial corpus to verify cleanly; only {} did (walker likely broken). Skipped:\n{}",
        tally.verified,
        tally.skipped.join("\n")
    );
}
