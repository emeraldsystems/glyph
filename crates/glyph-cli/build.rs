use std::env;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn is_dirty() -> bool {
    // Best-effort: if git isn't available, assume clean.
    let diff = Command::new("git").args(["diff", "--quiet"]).status();
    let diff_cached = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .status();
    match (diff, diff_cached) {
        (Ok(a), Ok(b)) => !(a.success() && b.success()),
        _ => false,
    }
}

fn main() {
    // Rebuild if the git state changes.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    let id = git(&["describe", "--tags", "--exact-match"])
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]));

    let mut full = pkg_version.clone();
    if let Some(mut id) = id {
        if is_dirty() {
            id.push_str("-dirty");
        }
        full = format!("{} ({})", pkg_version, id);
    }

    println!("cargo:rustc-env=GLYPH_BUILD_VERSION={}", full);

    keep_runtime_symbols_in_bins();
}

/// GLYPH-83: `glyph-cli run` (JIT) and `glyph run` need every runtime C
/// function (glyph_fmt_*, glyph_json_*, glyph_net_*, glyph_audio_*, ...)
/// present in the process image so the JIT can resolve externs by
/// process-wide symbol lookup (see
/// `glyph_backend::codegen::CodegenContext::jit_execute_i32_with_symbols`).
///
/// glyph-backend's build script links `libglyph_runtime.a` whole
/// (`static:+whole-archive,-bundle`), so every member reaches every final
/// link already - see the comment there for why the archive must not also
/// be force-loaded a second time from here (duplicate symbols on ELF).
///
/// That is sufficient on ELF and Windows. On macOS it is not: rustc
/// unconditionally passes `-Wl,-dead_strip` for executables, and ld64's
/// dead-code pass runs *after* force_load has pulled every member in, so it
/// deletes every force-loaded function nothing else references - right
/// back to a binary missing glyph_fmt_write_str et al. Verified empirically
/// (GLYPH-83 investigation notes) that `-exported_symbols_list` accepts glob
/// patterns and that ld64 treats every name it matches as a GC root even in
/// an executable. Every runtime/*.c function is named `glyph_*` with no
/// exceptions (checked against all RUNTIME_SOURCES files), so a single
/// `_glyph_*` wildcard is the pattern - zero per-symbol maintenance. The
/// tradeoff: everything **not** matching `_glyph_*` in glyph-cli/glyph
/// becomes a locally-bound symbol (Mach-O's `__private_extern__`) instead
/// of globally exported. That's a no-op for a plain CLI executable (nothing
/// dlopens/dlsyms into it; DWARF and local backtraces are untouched) and
/// doesn't affect symbols from *other* shared libraries.
///
/// `rustc-link-arg-bins` scopes the flags to this crate's own `[[bin]]`
/// targets (glyph-cli and glyph); test binaries don't need them.
fn keep_runtime_symbols_in_bins() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" => {
            let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
            let exports_list =
                std::path::PathBuf::from(&out_dir).join("glyph_runtime_exported_symbols.txt");
            std::fs::write(&exports_list, "_glyph_*\n")
                .expect("failed to write exported-symbols list for the runtime archive");
            println!(
                "cargo:rustc-link-arg-bins=-Wl,-exported_symbols_list,{}",
                exports_list.display()
            );
        }
        "windows" => {}
        // ELF platforms (linux, freebsd, android, ...). Linking the archive
        // whole puts every runtime function in the executable, but the JIT
        // resolves externs through `LLVMSearchForAddressOfSymbol`, i.e.
        // dlsym on the running process, which on ELF only sees the
        // *dynamic* symbol table - and an executable's own globals are not
        // placed there unless the link exports them (Mach-O exports them by
        // default, which is why macOS never needed this). `--export-dynamic`
        // publishes them; it also makes them GC roots, so rustc's
        // `--gc-sections` can't strip the force-loaded members. It applies
        // to every glyph-cli link target, not just the bins, because
        // cli_run_jit.rs resolves the same symbols inside the test binary.
        // `--no-gc-sections` on the bins is belt-and-braces on top.
        _ => {
            println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
            println!("cargo:rustc-link-arg-bins=-Wl,--no-gc-sections");
        }
    }
}
