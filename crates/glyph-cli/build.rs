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

    force_load_runtime_archive();
}

/// GLYPH-83: `glyph-cli run` (JIT) and `glyph run` both need every runtime
/// C function (glyph_fmt_*, glyph_json_*, glyph_net_*, glyph_audio_*, ...)
/// present in the `glyph-cli`/`glyph` process image so the JIT can resolve
/// externs by process-wide symbol lookup (see
/// `glyph_backend::codegen::CodegenContext::jit_execute_i32_with_symbols`).
///
/// glyph-backend's build script archives every runtime/*.c source into
/// `libglyph_runtime.a` and links it with a plain `-lglyph_runtime`, but a
/// plain static-archive link only pulls in the .o members that resolve a
/// symbol some other object file already references. Nothing in Rust calls
/// glyph_fmt_write_str/glyph_json_*/glyph_net_*/glyph_audio_* directly, so
/// those members get dropped and the symbols simply don't exist in the
/// binary. Force-loading the whole archive (linker-specific "keep every
/// member" flag) is the fix: it makes the *same* canonical list of runtime
/// objects glyph-backend's build.rs already compiles (`RUNTIME_SOURCES`)
/// the one source of truth for "what's in the binary" — a new runtime
/// function needs no separate registration anywhere.
///
/// The archive path comes from glyph-backend via Cargo's `links` metadata
/// forwarding (`DEP_GLYPH_RUNTIME_RUNTIME_ARCHIVE`, see
/// `glyph-backend/build.rs`), which only a *direct* dependent's build
/// script can read. `rustc-link-arg-bins` scopes the flag to this crate's
/// own `[[bin]]` targets (glyph-cli and glyph) so `cargo test` binaries
/// aren't bloated by it.
///
/// Force-loading alone isn't sufficient on macOS: rustc unconditionally
/// passes `-Wl,-dead_strip` for executables, and ld64's dead-code pass runs
/// *after* force_load has pulled every member in, so it promptly deletes
/// every force-loaded function nothing else references — right back to a
/// binary missing glyph_fmt_write_str et al. Verified empirically (see the
/// GLYPH-83 investigation notes) that `-exported_symbols_list` accepts glob
/// patterns and that ld64 treats every name it matches as a GC root even in
/// an executable, which survives dead_strip. Every runtime/*.c function is
/// named `glyph_*` with no exceptions (checked against all ten
/// RUNTIME_SOURCES files), so a single `_glyph_*` wildcard is the pattern —
/// still zero per-symbol maintenance. The tradeoff: everything **not**
/// matching `_glyph_*` in glyph-cli/glyph becomes a locally-bound symbol
/// (Mach-O's `__private_extern__`) instead of globally exported. That's a
/// no-op for a plain CLI executable (nothing dlopens/dlsyms into it, and
/// DWARF debug info and local backtraces are untouched either way) and
/// doesn't affect symbols pulled from *other* shared libraries (libSystem,
/// libLLVM, ...), which live outside this exports list entirely.
fn force_load_runtime_archive() {
    let Ok(archive) = env::var("DEP_GLYPH_RUNTIME_RUNTIME_ARCHIVE") else {
        // glyph-backend's build.rs always emits this key today; if it's
        // ever missing (e.g. a future refactor), fail loudly at build time
        // rather than silently shipping a `run` that segfaults again.
        panic!(
            "DEP_GLYPH_RUNTIME_RUNTIME_ARCHIVE not set; expected glyph-backend's build.rs \
             (links = \"glyph_runtime\") to forward the runtime archive path"
        );
    };
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" => {
            let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
            let exports_list =
                std::path::PathBuf::from(&out_dir).join("glyph_runtime_exported_symbols.txt");
            std::fs::write(&exports_list, "_glyph_*\n")
                .expect("failed to write exported-symbols list for the runtime archive");
            println!("cargo:rustc-link-arg-bins=-Wl,-force_load,{archive}");
            println!(
                "cargo:rustc-link-arg-bins=-Wl,-exported_symbols_list,{}",
                exports_list.display()
            );
            // runtime/glyph_audio.c's AudioQueue-based playback path (only
            // reachable, before this fix, via std/audio's AOT-linked
            // externs) is now force-loaded too, so its AudioToolbox import
            // needs to actually be linked rather than relying on it having
            // already been dropped as dead weight.
            println!("cargo:rustc-link-arg-bins=-framework");
            println!("cargo:rustc-link-arg-bins=AudioToolbox");
        }
        "windows" => {
            println!("cargo:rustc-link-arg-bins=/WHOLEARCHIVE:{archive}");
        }
        // ELF platforms (linux, freebsd, android, ...): --whole-archive
        // must wrap the archive itself, not just the -l flag. rustc does
        // not add `--gc-sections` by default here (unlike macOS's
        // unconditional `-dead_strip`), so force_load's equivalent
        // (--whole-archive) needs no dead-strip counterpart today; ask for
        // --no-gc-sections anyway so a future default change (or a
        // downstream RUSTFLAGS that enables it) can't reintroduce this bug
        // silently.
        _ => {
            println!("cargo:rustc-link-arg-bins=-Wl,--whole-archive");
            println!("cargo:rustc-link-arg-bins={archive}");
            println!("cargo:rustc-link-arg-bins=-Wl,--no-whole-archive");
            println!("cargo:rustc-link-arg-bins=-Wl,--no-gc-sections");
        }
    }
}
