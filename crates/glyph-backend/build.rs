use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Runtime C sources compiled into libglyph_runtime.a. Add new runtime
/// modules here (and only here).
const RUNTIME_SOURCES: &[&str] = &[
    "glyph_fmt",
    "glyph_json",
    "glyph_process",
    "glyph_time",
    "glyph_term",
    "glyph_net",
    "glyph_audio",
];

fn main() {
    // Get the output directory where cargo builds artifacts
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let runtime_lib = out_dir.join("libglyph_runtime.a");

    let mut objects = Vec::new();
    for name in RUNTIME_SOURCES {
        let src = PathBuf::from(format!("../../runtime/{}.c", name));
        if !src.exists() {
            panic!("Runtime library source not found at: {:?}", src);
        }

        let obj = out_dir.join(format!("{}.o", name));
        println!("cargo:warning=Compiling runtime library from {:?}", src);
        let status = Command::new("cc")
            .args(&[
                "-c",    // Compile only, don't link
                "-O2",   // Optimize
                "-fPIC", // Position-independent code for shared libraries
                "-Wall", // Enable warnings
            ])
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

    println!(
        "cargo:warning=Runtime library built successfully at {}",
        runtime_lib.display()
    );
}
