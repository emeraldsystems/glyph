use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Runtime C sources compiled into libglyph_runtime.a. Add new runtime
/// modules here (and only here).
const RUNTIME_SOURCES: &[&str] = &[
    "glyph_fmt",
    "glyph_io",
    "glyph_json",
    "glyph_process",
    "glyph_time",
    "glyph_term",
    "glyph_net",
    "glyph_audio",
    "glyph_thread",
    "glyph_mutex",
];

fn main() {
    // Get the output directory where cargo builds artifacts
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let runtime_lib = out_dir.join("libglyph_runtime.a");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_default();
    let runtime_sanitizer = env::var("GLYPH_RUNTIME_SANITIZER").ok();
    println!("cargo:rerun-if-env-changed=GLYPH_RUNTIME_SANITIZER");

    let mut objects = Vec::new();
    for name in RUNTIME_SOURCES {
        let src = PathBuf::from(format!("../../runtime/{}.c", name));
        if !src.exists() {
            panic!("Runtime library source not found at: {:?}", src);
        }

        let obj = out_dir.join(format!("{}.o", name));
        println!("cargo:warning=Compiling runtime library from {:?}", src);
        let mut cc = Command::new("cc");
        cc.args(&[
            "-c",    // Compile only, don't link
            "-O2",   // Optimize
            "-fPIC", // Position-independent code for shared libraries
            "-Wall", // Enable warnings
        ]);
        if let Some(sanitizer) = runtime_sanitizer.as_deref() {
            match sanitizer {
                "address" | "thread" => {
                    cc.arg(format!("-fsanitize={sanitizer}"));
                    cc.arg("-fno-omit-frame-pointer");
                }
                _ => panic!(
                    "unsupported GLYPH_RUNTIME_SANITIZER={sanitizer:?}; expected `address` or `thread`"
                ),
            }
        }
        if matches!(*name, "glyph_thread" | "glyph_mutex") {
            if matches!(target_os.as_str(), "macos" | "linux") {
                cc.arg("-pthread");
            }
        }
        if name == &"glyph_thread" {
            if profile != "release" {
                cc.arg("-DGLYPH_THREAD_ENABLE_TEST_HOOKS=1");
            }
        }
        let status = cc
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .status()
            .expect("Failed to execute cc compiler");

        if !status.success() {
            panic!(
                "Failed to compile {}.c. Make sure cc (clang/gcc) is installed.",
                name
            );
        }

        println!("cargo:rerun-if-changed=../../runtime/{}.c", name);
        if name == &"glyph_thread" {
            println!("cargo:rerun-if-changed=../../runtime/glyph_thread.h");
        } else if name == &"glyph_mutex" {
            println!("cargo:rerun-if-changed=../../runtime/glyph_mutex.h");
        }
        objects.push(obj);
    }

    // Create static library archive from the object files using ar
    println!("cargo:warning=Creating static library at {:?}", runtime_lib);
    let mut ar = Command::new("ar");
    ar.args(&["rcs"]) // r=insert, c=create, s=index
        .arg(&runtime_lib);
    for obj in &objects {
        ar.arg(obj);
    }
    let status = ar.status().expect("Failed to execute ar archiver");

    if !status.success() {
        panic!("Failed to create static library. Make sure ar is installed.");
    }

    // Tell cargo where to find the runtime library, and link it WHOLE,
    // exactly once, at every final link (GLYPH-83).
    //
    // A plain `static=glyph_runtime` only pulls in the .o members that
    // resolve a symbol some Rust object already references, so runtime
    // functions nothing in Rust calls directly (glyph_fmt_write_str,
    // glyph_json_*, glyph_net_*, glyph_audio_*, ...) were silently dropped
    // from the glyph-cli/glyph binaries and the JIT could not resolve them.
    // `+whole-archive` makes rustc emit the platform's keep-every-member
    // flag itself (-force_load on macOS, --whole-archive on ELF,
    // /WHOLEARCHIVE on MSVC). `-bundle` keeps the archive OUT of the rlib:
    // with the default bundling, the rlib carried a copy of these objects
    // and any second force-load of the .a (glyph-cli's build script used to
    // add one) produced duplicate-symbol link errors on ELF, because lld
    // had already extracted glyph_thread.o/glyph_mutex.o from the rlib to
    // satisfy thread_runtime.rs's externs before the whole-archive flag
    // was seen. With one unbundled, whole-archived copy there is nothing to
    // collide with, on any linker.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static:+whole-archive,-bundle=glyph_runtime");
    if matches!(target_os.as_str(), "macos" | "linux") {
        println!("cargo:rustc-link-lib=pthread");
    }
    if target_os == "macos" {
        // runtime/glyph_audio.c's AudioQueue playback path is now in every
        // link, so its AudioToolbox import has to resolve everywhere too.
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
    }

    // Expose the archive's path to direct dependents' build scripts via the
    // `links = "glyph_runtime"` manifest key (Cargo forwards this as
    // DEP_GLYPH_RUNTIME_RUNTIME_ARCHIVE) for tooling that wants it; the link
    // itself no longer needs it.
    println!("cargo:runtime_archive={}", runtime_lib.display());

    println!(
        "cargo:warning=Runtime library built successfully at {}",
        runtime_lib.display()
    );
}
