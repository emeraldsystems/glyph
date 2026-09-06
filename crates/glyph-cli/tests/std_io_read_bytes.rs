//! GLYPH-70: binary file reads.
//!
//! Glyph could read files before this, but only as text -- File::read_to_string
//! and nothing else -- so every binary format was unreachable. These tests
//! drive the new std/io::read_file_bytes end to end: a fixture with known
//! bytes is written from Rust, and a Glyph program reads it back and checks
//! the values itself, returning a distinct exit code per assertion.
//!
//! The error cases matter as much as the happy path here. A reader that
//! cannot tell "no such file" from "your buffer was too small" pushes that
//! diagnosis onto every caller.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Builds `main_src` as a project and runs it, returning the exit code.
///
/// Death by signal panics rather than returning: a crash is a failure, never
/// a skip. That distinction once let a SIGBUS pass as three green tests
/// (GLYPH-68), so it is worth restating in every harness.
fn build_and_run(dir: &Path, main_src: &str) -> i32 {
    fs::write(
        dir.join("glyph.toml"),
        "[package]\nname = \"iobytes\"\nversion = \"0.1.0\"\n\n\
         [[bin]]\nname = \"iobytes\"\npath = \"src/main.glyph\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.glyph"), main_src).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_glyph"))
        .arg("build")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "glyph build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(dir.join("target/debug/iobytes"))
        .current_dir(dir)
        .output()
        .unwrap();
    if let Some(sig) = run.status.signal() {
        panic!(
            "program killed by signal {sig}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
    run.status.code().unwrap()
}

/// Glyph has no `Vec::with_capacity`, and the runtime deliberately never
/// grows a Vec from C, so every caller sizes the buffer itself first.
const FILL: &str = r#"
fn fill_zeros(buf: &mut Vec<u8>, n: i64) -> i32 {
  let mut i: i64 = 0
  while i < n {
    buf.push(0)
    i = i + 1
  }
  ret 0
}
"#;

#[test]
fn reads_known_bytes_back_exactly() {
    let temp = TempDir::new().unwrap();
    let data: Vec<u8> = vec![0, 1, 2, 127, 128, 200, 254, 255];
    let path = temp.path().join("bytes.bin");
    fs::write(&path, &data).unwrap();

    let src = format!(
        r#"
import std
from std/io import read_file_bytes
from std/vec import Vec
{FILL}
fn main() -> i32 {{
  let mut buf: Vec<u8> = Vec::new()
  let _f = fill_zeros(&mut buf, 8)
  let got = read_file_bytes("{path}", &mut buf, 8)
  if got != 8 {{ ret 10 }}
  if buf[0] != 0 {{ ret 20 }}
  if buf[1] != 1 {{ ret 21 }}
  if buf[2] != 2 {{ ret 22 }}
  if buf[3] != 127 {{ ret 23 }}
  if buf[4] != 128 {{ ret 24 }}
  if buf[5] != 200 {{ ret 25 }}
  if buf[6] != 254 {{ ret 26 }}
  if buf[7] != 255 {{ ret 27 }}
  ret 0
}}
"#,
        path = path.display()
    );
    assert_eq!(build_and_run(temp.path(), &src), 0, "byte round trip failed");
}

#[test]
fn reports_a_short_read_at_end_of_file() {
    // Asking for more than the file holds is not an error -- it is how you
    // discover the length when you did not ask first. The count must be
    // honest rather than the requested max.
    //
    // The buffer used to be capped at 24 elements as a workaround for
    // GLYPH-72 (Vec<u8> corrupted the heap non-deterministically once it
    // grew past 32 elements via `push`). That bug is fixed, so this drives
    // the buffer past the old 32-element threshold with a real file larger
    // than 32 bytes, growing well beyond the once-unsafe zone.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("short.bin");
    let data: Vec<u8> = (0u8..40).collect();
    fs::write(&path, &data).unwrap();

    let src = format!(
        r#"
import std
from std/io import read_file_bytes
from std/vec import Vec
{FILL}
fn main() -> i32 {{
  let mut buf: Vec<u8> = Vec::new()
  let _f = fill_zeros(&mut buf, 64)
  let got = read_file_bytes("{path}", &mut buf, 64)
  if got != 40 {{ ret 10 }}
  if buf[0] != 0 {{ ret 20 }}
  if buf[1] != 1 {{ ret 21 }}
  if buf[32] != 32 {{ ret 22 }}
  if buf[39] != 39 {{ ret 23 }}
  // Past the data the caller's own zeros must still be there, untouched.
  if buf[40] != 0 {{ ret 30 }}
  if buf[63] != 0 {{ ret 31 }}
  ret 0
}}
"#,
        path = path.display()
    );
    assert_eq!(build_and_run(temp.path(), &src), 0, "short read misreported");
}

#[test]
fn distinguishes_missing_file_from_undersized_buffer() {
    // The whole point of separate codes. A caller wants to tell a missing
    // asset from a programming mistake on its own side.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("exists.bin");
    fs::write(&path, [1u8, 2, 3, 4]).unwrap();
    let missing = temp.path().join("nope.bin");

    let src = format!(
        r#"
import std
from std/io import read_file_bytes
from std/vec import Vec
{FILL}
fn main() -> i32 {{
  let mut small: Vec<u8> = Vec::new()
  let _f1 = fill_zeros(&mut small, 2)
  // Buffer holds 2 but 4 was requested: refused, not truncated.
  let undersized = read_file_bytes("{path}", &mut small, 4)
  if undersized != -2 {{ ret 10 }}

  let mut buf: Vec<u8> = Vec::new()
  let _f2 = fill_zeros(&mut buf, 4)
  let absent = read_file_bytes("{missing}", &mut buf, 4)
  if absent != -3 {{ ret 20 }}

  // A zero-length request is legal and reads nothing.
  let none = read_file_bytes("{path}", &mut buf, 0)
  if none != 0 {{ ret 30 }}
  ret 0
}}
"#,
        path = path.display(),
        missing = missing.display()
    );
    assert_eq!(build_and_run(temp.path(), &src), 0, "error codes not distinct");
}

#[test]
fn decodes_little_endian_i16_in_glyph() {
    // The acceptance test that matters for GLYPHAUDIO-25: proving the generic
    // byte reader is sufficient for binary numeric data WITHOUT a typed
    // reader in the runtime.
    //
    // Glyph has no bitwise operators and (today) no i16 type, but two's
    // complement is arithmetic, not bit twiddling: b0 + b1 * 256, then
    // subtract 65536 above the signed range.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("pcm.bin");
    // 0, 1, -1, 32767, -32768 as little-endian i16.
    let samples: [i16; 5] = [0, 1, -1, 32767, -32768];
    let mut bytes = Vec::new();
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    fs::write(&path, &bytes).unwrap();

    let src = format!(
        r#"
import std
from std/io import read_file_bytes
from std/vec import Vec
{FILL}
// Two's complement, little endian. Exact for every 16-bit value.
fn decode_i16(buf: &Vec<u8>, at: usize) -> i32 {{
  let raw: i32 = (buf[at] as i32) + (buf[at + 1] as i32) * 256
  if raw >= 32768 {{
    ret raw - 65536
  }}
  ret raw
}}

fn main() -> i32 {{
  let mut buf: Vec<u8> = Vec::new()
  let _f = fill_zeros(&mut buf, 10)
  let got = read_file_bytes("{path}", &mut buf, 10)
  if got != 10 {{ ret 10 }}

  if decode_i16(&buf, 0) != 0 {{ ret 20 }}
  if decode_i16(&buf, 2) != 1 {{ ret 21 }}
  if decode_i16(&buf, 4) != 0 - 1 {{ ret 22 }}
  if decode_i16(&buf, 6) != 32767 {{ ret 23 }}
  if decode_i16(&buf, 8) != 0 - 32768 {{ ret 24 }}
  ret 0
}}
"#,
        path = path.display()
    );
    assert_eq!(build_and_run(temp.path(), &src), 0, "i16 decode failed");
}
