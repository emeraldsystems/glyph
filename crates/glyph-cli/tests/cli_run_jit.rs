//! GLYPH-83: `glyph-cli run` (the JIT execution path — distinct from the
//! `glyph run` project tool, which builds and executes an AOT binary and
//! never hit this bug) segfaulted on any program that reached the
//! formatting runtime, including the README's own "Try It Out" command
//! (`glyph-cli run examples/std_hello/hello.glyph`). Root cause: JIT
//! execution registered a hand-maintained symbol table
//! (`crates/glyph-cli/src/main.rs::run()`) that never covered
//! glyph_fmt_write_*/glyph_json_*/glyph_net_*/glyph_audio_* — those
//! runtime/*.c object files were never even linked into the `glyph-cli`
//! binary, since nothing in Rust referenced them and a plain static-archive
//! link only pulls in .o members something else resolves. MCJIT patched
//! the unresolved call sites with a null address; the process jumped there
//! and segfaulted (exit 139), silently, with no diagnostic.
//!
//! The fix has two parts, covered by this file's two test groups:
//!
//! 1. `crates/glyph-cli/build.rs` now force-loads glyph-backend's entire
//!    runtime archive into the `glyph-cli`/`glyph` binaries and protects it
//!    from macOS's automatic `-dead_strip` pass, and
//!    `CodegenContext::jit_execute_i32_with_symbols`
//!    (crates/glyph-backend/src/codegen/emit.rs) resolves any extern not in
//!    its caller-supplied map via a process-wide symbol search, and — this
//!    is the "missing symbols fail loudly" half of the fix — verifies every
//!    *used* extern declaration resolves before ever creating the
//!    execution engine, turning what used to be a segfault into a named
//!    `Result::Err`.
//! 2. [`every_stdlib_extern_symbol_resolves_in_the_jit_symbol_source`]
//!    enumerates every extern name the embedded stdlib can cause to be
//!    declared and checks each one directly against
//!    `CodegenContext::jit_resolve_symbol`, so a future runtime function
//!    that regresses back into this bug (or a new stdlib extern nobody
//!    wired up) is caught here rather than by a user hitting a segfault.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::process::Command;

use glyph_backend::codegen::CodegenContext;
use glyph_core::ast::Item;
use glyph_frontend::std_modules;
use tempfile::TempDir;

// ---------------------------------------------------------------------
// Group 1: every stdlib extern resolves in the JIT's symbol source.
// ---------------------------------------------------------------------

/// Declared by the embedded stdlib purely so import/type resolution has a
/// signature to check `println`/`argv` calls against — every real call site
/// is intercepted during MIR lowering and rewritten into direct calls to
/// the actual runtime primitives (see
/// `glyph_frontend::mir_lower::call::lower_print_builtin` for `println` and
/// `codegen_sys_argv_value` for `argv`), so these three names are never
/// emitted as an actual call and legitimately do not need to resolve to
/// anything (glyph-backend/src/codegen/emit.rs's preflight check skips
/// exactly this case via `LLVMGetFirstUse`).
const DECLARED_BUT_NEVER_CALLED: &[&str] = &["println", "argv", "glyph_print"];

/// print/println lower straight to these nine runtime/glyph_fmt.c externs
/// (see `crates/glyph-frontend/src/mir_lower/mod.rs`'s `fmt_externs`
/// table), registered directly in MIR lowering rather than through a
/// `std_modules()` AST declaration — `std_modules()` alone would miss
/// exactly the functions whose absence caused GLYPH-83 (println's crash).
/// Mirrored here since mir_lower doesn't expose this table publicly.
const PRINT_RUNTIME_EXTERNS: &[&str] = &[
    "glyph_fmt_write_i32",
    "glyph_fmt_write_u32",
    "glyph_fmt_write_i64",
    "glyph_fmt_write_u64",
    "glyph_fmt_write_bool",
    "glyph_fmt_write_char",
    "glyph_fmt_write_str",
    "glyph_fmt_write_f32",
    "glyph_fmt_write_f64",
];

// Every `runtime/*.c` function (i.e. every name starting with `glyph_`,
// minus the three declared-but-never-called above) that `std_modules()` or
// `PRINT_RUNTIME_EXTERNS` can cause to be declared in generated MIR, as of
// this writing. `jit_resolve_symbol` only finds a `glyph_*` symbol if its
// containing runtime/*.c object file is actually linked into *this test
// binary* — unlike `glyph-cli`/`glyph`, this test binary is a `[[test]]`
// target, which `build.rs::force_load_runtime_archive` deliberately does
// not force-load into (see that function's doc comment). So, exactly like
// `crates/glyph-cli/src/thread_runtime.rs` already does for the
// thread/mutex runtime, each name below is referenced directly by an
// `extern "C"` declaration to force its object file to be linked in here
// too. The fake zero-argument signature is fine: nothing ever calls these
// through the declaration, only takes their address.
//
// `every_stdlib_extern_symbol_resolves_in_the_jit_symbol_source` below
// cross-checks this list against `std_modules()` + `PRINT_RUNTIME_EXTERNS`,
// so a new runtime function that stdlib code starts calling without a
// matching entry here fails this test with a clear diff instead of
// silently going unchecked.
unsafe extern "C" {
    fn glyph_fmt_write_i32();
    fn glyph_fmt_write_u32();
    fn glyph_fmt_write_i64();
    fn glyph_fmt_write_u64();
    fn glyph_fmt_write_bool();
    fn glyph_fmt_write_char();
    fn glyph_fmt_write_str();
    fn glyph_fmt_write_f32();
    fn glyph_fmt_write_f64();

    fn glyph_io_ignore_sigpipe();
    fn glyph_io_read_file_bytes();
    fn glyph_io_read_line();

    fn glyph_file_size();
    fn glyph_net_accept();
    fn glyph_net_bind();
    fn glyph_net_close();
    fn glyph_net_get_last_error();
    fn glyph_net_listen();
    fn glyph_net_local_port();
    fn glyph_net_set_reuse_addr();
    fn glyph_net_tcp_connect();
    fn glyph_net_tcp_recv();
    fn glyph_net_tcp_send();
    fn glyph_net_tcp_send_file();
    fn glyph_net_tcp_socket();
    fn glyph_net_udp_bind();
    fn glyph_net_udp_recv();
    fn glyph_net_udp_send_to();
    fn glyph_net_udp_socket();

    fn glyph_process_run();

    fn glyph_byte_at();
    fn glyph_string_char_at();
    fn glyph_string_from_byte();
    fn glyph_string_from_char();
    fn glyph_string_from_f64();
    fn glyph_string_from_i32();
    fn glyph_string_index_of();

    fn glyph_term_clear_line();
    fn glyph_term_enter_ui_session();
    fn glyph_term_flush();
    fn glyph_term_move_to();
    fn glyph_term_poll_event();
    fn glyph_term_session_end();
    fn glyph_term_stdout();
    fn glyph_term_write_str();

    fn glyph_time_monotonic_ns();
    fn glyph_time_now();
    fn glyph_time_sleep_ms();
    fn glyph_time_sleep_until_ns();
    fn glyph_time_sleep_us();
    fn glyph_time_to_human_readable();
}

// glyph_audio.c's playback path (glyph_audio_out_*) calls into
// AudioToolbox's AudioQueue APIs on macOS/iOS. Referencing those symbols
// below pulls glyph_audio.o's *other* undefined references in too, so this
// test binary needs the same framework link `build.rs::force_load_runtime_archive`
// adds for the real glyph-cli/glyph binaries — scoped to just this
// translation unit via `#[link(...)]` rather than a global build.rs change,
// since forcing every runtime object into every test binary is exactly
// what force-loading deliberately avoids (see that function's doc comment).
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    link(name = "AudioToolbox", kind = "framework")
)]
unsafe extern "C" {
    fn glyph_audio_out_close();
    fn glyph_audio_out_open();
    fn glyph_audio_out_write();
    fn glyph_audio_wav_close();
    fn glyph_audio_wav_open();
    fn glyph_audio_wav_write();
}

/// Forces the linker to retain every runtime/*.c object file this test
/// needs (see the big `unsafe extern "C"` block above), and hands back the
/// resulting name -> address table for the comprehensiveness check.
fn force_link_runtime_symbols() -> HashMap<&'static str, u64> {
    macro_rules! addr_of {
        ($name:ident) => {
            ($name as unsafe extern "C" fn() as usize as u64)
        };
    }
    HashMap::from([
        ("glyph_fmt_write_i32", addr_of!(glyph_fmt_write_i32)),
        ("glyph_fmt_write_u32", addr_of!(glyph_fmt_write_u32)),
        ("glyph_fmt_write_i64", addr_of!(glyph_fmt_write_i64)),
        ("glyph_fmt_write_u64", addr_of!(glyph_fmt_write_u64)),
        ("glyph_fmt_write_bool", addr_of!(glyph_fmt_write_bool)),
        ("glyph_fmt_write_char", addr_of!(glyph_fmt_write_char)),
        ("glyph_fmt_write_str", addr_of!(glyph_fmt_write_str)),
        ("glyph_fmt_write_f32", addr_of!(glyph_fmt_write_f32)),
        ("glyph_fmt_write_f64", addr_of!(glyph_fmt_write_f64)),
        ("glyph_audio_out_close", addr_of!(glyph_audio_out_close)),
        ("glyph_audio_out_open", addr_of!(glyph_audio_out_open)),
        ("glyph_audio_out_write", addr_of!(glyph_audio_out_write)),
        ("glyph_audio_wav_close", addr_of!(glyph_audio_wav_close)),
        ("glyph_audio_wav_open", addr_of!(glyph_audio_wav_open)),
        ("glyph_audio_wav_write", addr_of!(glyph_audio_wav_write)),
        ("glyph_io_ignore_sigpipe", addr_of!(glyph_io_ignore_sigpipe)),
        ("glyph_io_read_file_bytes", addr_of!(glyph_io_read_file_bytes)),
        ("glyph_io_read_line", addr_of!(glyph_io_read_line)),
        ("glyph_file_size", addr_of!(glyph_file_size)),
        ("glyph_net_accept", addr_of!(glyph_net_accept)),
        ("glyph_net_bind", addr_of!(glyph_net_bind)),
        ("glyph_net_close", addr_of!(glyph_net_close)),
        ("glyph_net_get_last_error", addr_of!(glyph_net_get_last_error)),
        ("glyph_net_listen", addr_of!(glyph_net_listen)),
        ("glyph_net_local_port", addr_of!(glyph_net_local_port)),
        ("glyph_net_set_reuse_addr", addr_of!(glyph_net_set_reuse_addr)),
        ("glyph_net_tcp_connect", addr_of!(glyph_net_tcp_connect)),
        ("glyph_net_tcp_recv", addr_of!(glyph_net_tcp_recv)),
        ("glyph_net_tcp_send", addr_of!(glyph_net_tcp_send)),
        ("glyph_net_tcp_send_file", addr_of!(glyph_net_tcp_send_file)),
        ("glyph_net_tcp_socket", addr_of!(glyph_net_tcp_socket)),
        ("glyph_net_udp_bind", addr_of!(glyph_net_udp_bind)),
        ("glyph_net_udp_recv", addr_of!(glyph_net_udp_recv)),
        ("glyph_net_udp_send_to", addr_of!(glyph_net_udp_send_to)),
        ("glyph_net_udp_socket", addr_of!(glyph_net_udp_socket)),
        ("glyph_process_run", addr_of!(glyph_process_run)),
        ("glyph_byte_at", addr_of!(glyph_byte_at)),
        ("glyph_string_char_at", addr_of!(glyph_string_char_at)),
        ("glyph_string_from_byte", addr_of!(glyph_string_from_byte)),
        ("glyph_string_from_char", addr_of!(glyph_string_from_char)),
        ("glyph_string_from_f64", addr_of!(glyph_string_from_f64)),
        ("glyph_string_from_i32", addr_of!(glyph_string_from_i32)),
        ("glyph_string_index_of", addr_of!(glyph_string_index_of)),
        ("glyph_term_clear_line", addr_of!(glyph_term_clear_line)),
        (
            "glyph_term_enter_ui_session",
            addr_of!(glyph_term_enter_ui_session),
        ),
        ("glyph_term_flush", addr_of!(glyph_term_flush)),
        ("glyph_term_move_to", addr_of!(glyph_term_move_to)),
        ("glyph_term_poll_event", addr_of!(glyph_term_poll_event)),
        ("glyph_term_session_end", addr_of!(glyph_term_session_end)),
        ("glyph_term_stdout", addr_of!(glyph_term_stdout)),
        ("glyph_term_write_str", addr_of!(glyph_term_write_str)),
        ("glyph_time_monotonic_ns", addr_of!(glyph_time_monotonic_ns)),
        ("glyph_time_now", addr_of!(glyph_time_now)),
        ("glyph_time_sleep_ms", addr_of!(glyph_time_sleep_ms)),
        (
            "glyph_time_sleep_until_ns",
            addr_of!(glyph_time_sleep_until_ns),
        ),
        ("glyph_time_sleep_us", addr_of!(glyph_time_sleep_us)),
        (
            "glyph_time_to_human_readable",
            addr_of!(glyph_time_to_human_readable),
        ),
    ])
}

/// Every extern symbol name `std_modules()` declares plus
/// `PRINT_RUNTIME_EXTERNS`, deduplicated — the full set of runtime symbol
/// names the embedded stdlib can cause to appear in generated MIR.
fn all_stdlib_extern_symbols() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for module in std_modules().values() {
        for item in &module.items {
            if let Item::ExternFunction(decl) = item {
                let symbol = decl
                    .link_name
                    .clone()
                    .unwrap_or_else(|| decl.name.0.clone());
                names.insert(symbol);
            }
        }
    }
    for name in PRINT_RUNTIME_EXTERNS {
        names.insert((*name).to_string());
    }
    names
}

#[test]
fn every_stdlib_extern_symbol_resolves_in_the_jit_symbol_source() {
    let names = all_stdlib_extern_symbols();
    assert!(
        names.len() > 60,
        "expected the embedded stdlib to declare a substantial number of \
         extern runtime symbols (glyph_* and libc); found only {} ({:?}) — \
         did std_modules() or the print-runtime table change shape?",
        names.len(),
        names
    );

    let force_linked = force_link_runtime_symbols();

    // Comprehensiveness check first: if stdlib code starts calling a new
    // glyph_* runtime function, this must fail here (with a clear diff)
    // rather than silently skip validating it below.
    let expected_glyph_prefixed: BTreeSet<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| n.starts_with("glyph_") && !DECLARED_BUT_NEVER_CALLED.contains(n))
        .collect();
    let actually_force_linked: BTreeSet<&str> = force_linked.keys().copied().collect();
    assert_eq!(
        expected_glyph_prefixed, actually_force_linked,
        "the embedded stdlib's glyph_* runtime externs no longer match the \
         `unsafe extern \"C\"` block force-linked into this test binary — \
         add the new name(s) to both the extern block and \
         force_link_runtime_symbols() in crates/glyph-cli/tests/cli_run_jit.rs"
    );

    let empty_overrides = HashMap::new();
    let mut unresolved = Vec::new();
    for name in &names {
        if DECLARED_BUT_NEVER_CALLED.contains(&name.as_str()) {
            continue;
        }
        if CodegenContext::jit_resolve_symbol(name, &empty_overrides).is_none() {
            unresolved.push(name.clone());
        }
    }
    assert!(
        unresolved.is_empty(),
        "runtime symbol(s) declared by the embedded stdlib do not resolve in \
         the JIT's process-wide symbol source — `glyph-cli run` would \
         segfault (GLYPH-83) on any program that calls them: {unresolved:?}"
    );
}

// ---------------------------------------------------------------------
// Group 2: end-to-end `glyph-cli run` coverage.
// ---------------------------------------------------------------------

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// The README's own "Try It Out" command. This is the exact program that
/// used to segfault (exit 139, no output) before the GLYPH-83 fix.
#[test]
fn glyph_cli_run_prints_hello_world_for_std_hello_example() {
    let hello = workspace_root().join("examples/std_hello/hello.glyph");
    assert!(
        hello.is_file(),
        "expected examples/std_hello/hello.glyph to exist at {}",
        hello.display()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_glyph-cli"))
        .arg("run")
        .arg(&hello)
        .output()
        .expect("failed to run glyph-cli");

    assert!(
        output.status.success(),
        "glyph-cli run exited with {:?} (signal: {:?})\nstdout: {}\nstderr: {}",
        output.status.code(),
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                output.status.signal()
            }
            #[cfg(not(unix))]
            {
                None::<i32>
            }
        },
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello world"
    );
}

/// A small std/json/parser program run via `glyph-cli run`: the second
/// report in GLYPH-83 was that JSON parsing crashed the same way println
/// did (both ultimately call into runtime/*.c functions the JIT couldn't
/// resolve — glyph_fmt_write_* for println, glyph_string_from_* /
/// glyph_byte_at for the JSON parser's string handling).
#[test]
fn glyph_cli_run_parses_json_without_crashing() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("main.glyph"),
        r#"
from std import println
from std/json import JsonValue, ParseResult
from std/json/parser import parse

fn main() -> i32 {
    let parsed = parse("{\"a\": [1, 2, 3], \"b\": true}")
    ret match parsed {
        Ok(v) => match v {
            Object(_fields) => {
                println("parsed ok")
                ret 0
            },
            _ => 1,
        },
        Err(_e) => 2,
    }
}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glyph-cli"))
        .arg("run")
        .arg(dir.path().join("main.glyph"))
        .output()
        .expect("failed to run glyph-cli");

    assert!(
        output.status.success(),
        "glyph-cli run exited with {:?} (signal: {:?})\nstdout: {}\nstderr: {}",
        output.status.code(),
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                output.status.signal()
            }
            #[cfg(not(unix))]
            {
                None::<i32>
            }
        },
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "parsed ok"
    );
}

/// The other half of the fix: a genuinely unresolvable extern must fail
/// with a named, catchable error — never a segfault. Exercised through the
/// real `glyph-cli run` binary (a signal-based crash here would show up as
/// `output.status.code()` being `None` with a `signal()` of 11, not as a
/// normal nonzero exit code with a message).
#[test]
fn glyph_cli_run_reports_an_unresolvable_extern_instead_of_crashing() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("main.glyph"),
        r#"
extern "C" fn glyph_83_symbol_that_will_never_exist(x: i32) -> i32;

fn main() -> i32 {
    ret glyph_83_symbol_that_will_never_exist(1)
}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glyph-cli"))
        .arg("run")
        .arg(dir.path().join("main.glyph"))
        .output()
        .expect("failed to run glyph-cli");

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            None,
            "glyph-cli run crashed by signal instead of returning a named \
             error; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        !output.status.success(),
        "expected a nonzero exit for an unresolvable extern"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unresolved external symbol")
            && stderr.contains("glyph_83_symbol_that_will_never_exist"),
        "expected a named unresolved-symbol error, got stderr: {stderr}"
    );
}
