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

    // Tell cargo where to find the runtime library
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=glyph_runtime");
    if matches!(target_os.as_str(), "macos" | "linux") {
        println!("cargo:rustc-link-lib=pthread");
    }

    // Expose the archive's path to direct dependents' build scripts via the
    // `links = "glyph_runtime"` manifest key (Cargo forwards this as
    // DEP_GLYPH_RUNTIME_RUNTIME_ARCHIVE). A plain `-lglyph_runtime` only
    // pulls in the .o members that resolve a symbol some other object
    // already references, so runtime functions nothing in Rust calls
    // directly (e.g. glyph_fmt_write_str, glyph_json_*, glyph_net_*,
    // glyph_audio_*) get silently dropped from the final binary. glyph-cli
    // reads this path to force-load the *entire* archive into its binaries,
    // so every runtime symbol is present in-process for the JIT to resolve
    // (GLYPH-83).
    println!("cargo:runtime_archive={}", runtime_lib.display());

    println!(
        "cargo:warning=Runtime library built successfully at {}",
        runtime_lib.display()
    );
}
