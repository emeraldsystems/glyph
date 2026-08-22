//! End-to-end test for the glyph.toml `[link]` section: a project that
//! declares `libs = ["m"]` can call libm externs and build/run through the
//! `glyph` project tool. (On macOS libm lives in libSystem, so -lm is
//! accepted and redundant; on Linux it is required.)

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn project_with_link_section_builds_and_runs() {
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }

    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "linkdemo"
version = "0.1.0"

[[bin]]
name = "linkdemo"
path = "src/main.glyph"

[link]
libs = ["m"]
"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.glyph"),
        r#"extern "C" fn sqrt(x: f64) -> f64;
extern "C" fn sin(x: f64) -> f64;

fn main() -> i32 {
  let r: f64 = sqrt(16.0);
  if r != 4.0 { ret 1 }
  if sin(0.0) != 0.0 { ret 2 }
  ret 0
}
"#,
    )
    .unwrap();

    let glyph_bin = env!("CARGO_BIN_EXE_glyph");

    let build = Command::new(glyph_bin)
        .arg("build")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "glyph build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = root.join("target/debug/linkdemo");
    assert!(exe.exists(), "expected built binary at {}", exe.display());

    let run = Command::new(&exe).output().unwrap();
    assert_eq!(
        run.status.code(),
        Some(0),
        "linkdemo exited nonzero: {:?}",
        run.status
    );
}

#[test]
fn project_without_link_section_still_builds() {
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return;
    }

    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "plain"
version = "0.1.0"

[[bin]]
name = "plain"
path = "src/main.glyph"
"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.glyph"),
        "fn main() -> i32 {\n  ret 0\n}\n",
    )
    .unwrap();

    let glyph_bin = env!("CARGO_BIN_EXE_glyph");
    let build = Command::new(glyph_bin)
        .arg("build")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "glyph build failed:\nstderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}
