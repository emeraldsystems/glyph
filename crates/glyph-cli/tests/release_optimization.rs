//! Release must optimize executable code while preserving ownership and ordinary
//! IEEE arithmetic. These tests use the project CLI, so profile plumbing matters.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn skip_run() -> bool {
    std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok()
}

fn project(source: &str, link: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src/main.glyph"), source).unwrap();
    fs::write(
        temp.path().join("glyph.toml"),
        format!(
            "[package]\nname = \"release_probe\"\nversion = \"0.1.0\"\n\n\
             [[bin]]\nname = \"release_probe\"\npath = \"src/main.glyph\"\n{link}"
        ),
    )
    .unwrap();
    temp
}

fn successful(output: Output, action: &str) -> Output {
    assert!(
        output.status.success(),
        "{action} failed ({:?}):\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn glyph(root: &Path, verb: &str, release: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glyph"));
    command.arg(verb).arg("--verbose").current_dir(root);
    if release {
        command.arg("--release");
    }
    let output = successful(command.output().unwrap(), &format!("glyph {verb}"));
    let expected = if release { "O2" } else { "O0" };
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("[glyph] optimization: {expected}")),
        "missing {expected} profile log: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn executable(root: &Path, release: bool) -> std::path::PathBuf {
    root.join("target")
        .join(if release { "release" } else { "debug" })
        .join("release_probe")
}

// Whole Mach-O file sizes are page-aligned, so compare actual executable code,
// not container padding or symbols. No disassembler or additional crate needed.
fn text_section(image: &[u8]) -> &[u8] {
    let little = if image.starts_with(b"\x7fELF") {
        assert_eq!(image[4], 2, "expected a 64-bit ELF executable");
        image[5] == 1
    } else {
        assert!(
            image.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
                || image.starts_with(&[0xfe, 0xed, 0xfa, 0xcf]),
            "expected a 64-bit ELF or Mach-O executable"
        );
        image[0] == 0xcf
    };
    let word = |offset: usize, width: usize| -> usize {
        let mut value = 0_u64;
        for i in 0..width {
            let shift = if little { i } else { width - 1 - i } * 8;
            value |= u64::from(image[offset + i]) << shift;
        }
        usize::try_from(value).unwrap()
    };
    if image.starts_with(b"\x7fELF") {
        let table = word(40, 8);
        let stride = word(58, 2);
        let names = table + word(62, 2) * stride;
        let strings = word(names + 24, 8);
        for i in 0..word(60, 2) {
            let section = table + i * stride;
            let name = strings + word(section, 4);
            if image[name..].starts_with(b".text\0") {
                let start = word(section + 24, 8);
                return &image[start..start + word(section + 32, 8)];
            }
        }
    } else {
        let mut command = 32;
        for _ in 0..word(16, 4) {
            if word(command, 4) == 0x19 {
                // LC_SEGMENT_64
                for i in 0..word(command + 64, 4) {
                    let section = command + 72 + i * 80;
                    if image[section..].starts_with(b"__text\0") {
                        let start = word(section + 48, 4);
                        return &image[start..start + word(section + 40, 8)];
                    }
                }
            }
            command += word(command + 4, 4);
        }
    }
    panic!("executable has no text section")
}

#[test]
fn release_changes_machine_code_and_build_and_run_keep_results() {
    if skip_run() {
        return;
    }
    let mut source = String::from(
        "extern \"C\" fn putchar(c: i32) -> i32;\n\
         fn main() -> i32 {\nlet mut x: i64 = 3\n",
    );
    // O0 materializes these locals; O2 can remove the redundant arithmetic.
    for i in 0..256 {
        source.push_str(&format!("let v{i}: i64 = x + {i}\nx = v{i} - {i}\n"));
    }
    source.push_str(
        "if x != 3 { ret 1 }\n\
         let mut i: i64 = 0\nlet mut sum: i64 = 0\n\
         while i < 1024 { sum = sum + i; i = i + 1 }\n\
         if sum != 523776 { ret 2 }\n\
         putchar(82)\nputchar(10)\nret 0\n}\n",
    );
    let temp = project(&source, "");
    for release in [false, true] {
        glyph(temp.path(), "build", release);
        let output = successful(
            Command::new(executable(temp.path(), release))
                .output()
                .unwrap(),
            "built arithmetic executable",
        );
        assert_eq!(output.stdout, b"R\n");
    }
    let debug = fs::read(executable(temp.path(), false)).unwrap();
    let release = fs::read(executable(temp.path(), true)).unwrap();
    let debug_text = text_section(&debug);
    let release_text = text_section(&release);
    assert!(
        release_text.len() + 1024 < debug_text.len(),
        "release must remove meaningful machine code: O0={} bytes, O2={} bytes",
        debug_text.len(),
        release_text.len()
    );
    assert_ne!(
        debug_text, release_text,
        "only the profile directory changed"
    );
    // Exercise the separate `run --release` entry point as well as `build`.
    assert_eq!(glyph(temp.path(), "run", false).stdout, b"R\n");
    assert_eq!(glyph(temp.path(), "run", true).stdout, b"R\n");
}

#[test]
fn release_preserves_indexed_float_loops_and_owned_branch_reassignment() {
    if skip_run() {
        return;
    }
    let temp = project(
        r#"
from std/vec import Vec
from std/string import byte_at

fn bump(xs: &mut Vec<f64>, slot: usize) {
  xs[slot] = xs[slot] * 2.0 + 0.5
}

fn owned_round(round: i32) -> i32 {
  let mut names: Vec<String> = Vec::new()
  let mut i: i32 = 0
  while i < 32 {
    let mut name = String::from_str("initial")
    if (round + i) % 2 == 0 {
      name = String::from_str("alpha")
    } else {
      name = String::from_str("omega")
    }
    names.push(name)
    i = i + 1
  }
  i = 0
  while i < 32 {
    let slot: usize = i as usize
    let before: str = names[slot]
    if before.len() != 5 { ret 1 }
    if (round + i) % 2 == 0 {
      if byte_at(before, 0) != 97 { ret 2 }
      names[slot] = String::from_str("even")
    } else {
      if byte_at(before, 0) != 111 { ret 3 }
      names[slot] = String::from_str("odd")
    }
    let after: str = names[slot]
    if (round + i) % 2 == 0 {
      if after.len() != 4 { ret 4 }
    } else {
      if after.len() != 3 { ret 5 }
    }
    i = i + 1
  }
  ret 0
}

fn main() -> i32 {
  let mut xs: Vec<f64> = Vec::new()
  let mut i: i32 = 0
  while i < 64 {
    xs.push((i as f64) * 0.25)
    i = i + 1
  }
  let mut sum: f64 = 0.0
  i = 0
  while i < 64 {
    let slot: usize = i as usize
    bump(&mut xs, slot)
    sum = sum + xs[slot]
    i = i + 1
  }
  if sum != 1040.0 { ret 10 }
  if xs[0] != 0.5 { ret 11 }
  if xs[63] != 32.0 { ret 12 }
  i = 0
  while i < 40 {
    let result: i32 = owned_round(i)
    if result != 0 { ret result }
    i = i + 1
  }
  ret 0
}
"#,
        "",
    );
    for release in [false, true] {
        glyph(temp.path(), "run", release);
    }
}

#[test]
fn release_preserves_nested_struct_and_tuple_returns() {
    if skip_run() {
        return;
    }
    let temp = project(
        r#"
from std/string import byte_at

struct Inner { label: String, gain: f64 }
struct Outer { left: Inner, right: Inner }

fn make_inner(label: String, gain: f64) -> Inner {
  ret Inner { label: label, gain: gain }
}

fn make_outer() -> Outer {
  let left = make_inner(String::from_str("left"), 0.25)
  let right = make_inner(String::from_str("right"), 0.75)
  ret Outer { left: left, right: right }
}

fn pair() -> (i32, String) {
  ret (42, String::from_str("tuple"))
}

fn wrap_pair() -> (i32, String) {
  let value = pair()
  ret value
}

fn main() -> i32 {
  let mut i: i32 = 0
  while i < 40 {
    let outer = make_outer()
    let left: str = outer.left.label
    let right: str = outer.right.label
    if left.len() != 4 { ret 1 }
    if right.len() != 5 { ret 2 }
    if byte_at(left, 0) != 108 { ret 3 }
    if byte_at(right, 4) != 116 { ret 4 }
    if outer.left.gain + outer.right.gain != 1.0 { ret 5 }
    let value = wrap_pair()
    if value.0 != 42 { ret 6 }
    let text: str = value.1
    if text.len() != 5 { ret 7 }
    if byte_at(text, 4) != 101 { ret 8 }
    i = i + 1
  }
  ret 0
}
"#,
        "",
    );
    for release in [false, true] {
        glyph(temp.path(), "run", release);
    }
}

#[test]
fn release_keeps_ieee_roundoff_signed_zero_nonfinite_and_separate_operations() {
    if skip_run() {
        return;
    }
    let temp = project(
        r#"
extern "C" fn ieee_input(index: i32) -> f64;
extern "C" fn ieee_input_f32(index: i32) -> f32;
extern "C" fn ieee_isnan(value: f64) -> i32;
extern "C" fn ieee_isinf(value: f64) -> i32;
extern "C" fn ieee_signbit(value: f64) -> i32;

fn main() -> i32 {
  let big: f64 = ieee_input(0)
  let negative: f64 = ieee_input(1)
  let one: f64 = ieee_input(2)
  if (big + negative) + one != 1.0 { ret 1 }
  if big + (negative + one) != 0.0 { ret 2 }

  let zero: f64 = ieee_input(3)
  let negative_zero: f64 = ieee_input(4)
  if ieee_signbit(negative_zero * one) == 0 { ret 3 }
  if ieee_signbit(negative_zero + zero) != 0 { ret 4 }
  let inverse: f64 = one / negative_zero
  if ieee_isinf(inverse) == 0 { ret 5 }
  if ieee_signbit(inverse) == 0 { ret 6 }

  let infinity: f64 = ieee_input(5)
  let nan: f64 = ieee_input(6)
  if ieee_isnan(nan * zero) == 0 { ret 7 }
  if ieee_isnan(infinity - infinity) == 0 { ret 8 }
  if ieee_isnan(infinity * zero) == 0 { ret 9 }
  if ieee_isinf(infinity + one) == 0 { ret 10 }
  if ieee_isnan(nan + one) == 0 { ret 11 }

  // Each product rounds to 1 before subtracting 1. A fused operation is
  // a small negative value instead; external inputs prevent constant folding.
  let a: f64 = ieee_input(7)
  let b: f64 = ieee_input(8)
  let minus_one: f64 = ieee_input(9)
  if a * b + minus_one != 0.0 { ret 12 }
  let af: f32 = ieee_input_f32(0)
  let bf: f32 = ieee_input_f32(1)
  let cf: f32 = ieee_input_f32(2)
  if af * bf + cf != 0.0 { ret 13 }
  ret 0
}
"#,
        "\n[link]\nlibs = [\"ieee_probe\", \"m\"]\nsearch_paths = [\".\"]\n",
    );
    // C99 classification macros are portable even where libc does not export
    // an `isnan` or `signbit` symbol. The separately compiled input function
    // also keeps the optimizer from replacing these with constant expressions.
    fs::write(
        temp.path().join("ieee.c"),
        r#"
#include <math.h>
double ieee_input(int index) {
    const double values[] = { 1e16, -1e16, 1.0, 0.0, -0.0, INFINITY, NAN,
                             1.0000000000000002, 0.9999999999999998, -1.0 };
    return values[index];
}
float ieee_input_f32(int index) {
    const float values[] = { 1.0000001192092896f, 0.9999998807907104f, -1.0f };
    return values[index];
}
int ieee_isnan(double value) { return isnan(value) != 0; }
int ieee_isinf(double value) { return isinf(value) != 0; }
int ieee_signbit(double value) { return signbit(value) != 0; }
"#,
    )
    .unwrap();
    successful(
        Command::new("clang")
            .args(["-std=c99", "-c", "ieee.c", "-o", "ieee.o"])
            .current_dir(temp.path())
            .output()
            .unwrap(),
        "C IEEE helper compilation",
    );
    successful(
        Command::new("ar")
            .args(["crs", "libieee_probe.a", "ieee.o"])
            .current_dir(temp.path())
            .output()
            .unwrap(),
        "C IEEE helper archive",
    );
    for release in [false, true] {
        glyph(temp.path(), "run", release);
    }
}
