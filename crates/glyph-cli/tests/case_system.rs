use std::fs;
use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn build_with_local_path_dependency_succeeds() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path();
    let lib_dir = workspace.join("my_lib");
    let app_dir = workspace.join("my_app");

    fs::create_dir_all(&lib_dir).unwrap();
    fs::create_dir_all(&app_dir).unwrap();

    write_file(
        &lib_dir.join("glyph.toml"),
        r#"[package]
name = "my_lib"
version = "0.1.0"

[lib]
path = "src/lib.glyph"
"#,
    );
    write_file(
        &lib_dir.join("src/lib.glyph"),
        r#"from utils import helper

fn greet() -> i32 {
  ret helper()
}
"#,
    );
    write_file(
        &lib_dir.join("src/utils.glyph"),
        r#"fn helper() -> i32 {
  ret 7
}
"#,
    );

    write_file(
        &app_dir.join("glyph.toml"),
        r#"[package]
name = "my_app"
version = "0.1.0"

[dependencies]
my_lib = { path = "../my_lib" }

[[bin]]
name = "my_app"
path = "src/main.glyph"
"#,
    );
    write_file(
        &app_dir.join("src/main.glyph"),
        r#"from std import println
from my_lib import greet

fn main() -> i32 {
  greet()
  println("hi")
  ret 0
}
"#,
    );

    let mut cmd = cargo_bin_cmd!("glyph");
    cmd.current_dir(&app_dir).arg("build").assert().success();

    let exe = app_dir.join("target").join("debug").join("my_app");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hi"));
}

#[test]
fn duplicate_helpers_across_cases_and_library_entry_aliases_execute_correctly() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path();
    let lib_dir = workspace.join("my_lib");
    let app_dir = workspace.join("my_app");
    write_file(
        &lib_dir.join("glyph.toml"),
        "[package]\nname = \"my_lib\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/api.glyph\"\n",
    );
    write_file(
        &lib_dir.join("src/api.glyph"),
        "from utils import helper\nfn answer() -> i32 { ret helper() }\n",
    );
    write_file(
        &lib_dir.join("src/utils.glyph"),
        "fn helper() -> i32 { ret 2 }",
    );
    write_file(
        &app_dir.join("glyph.toml"),
        "[package]\nname = \"my_app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmy_lib = { path = \"../my_lib\" }\n\n[[bin]]\nname = \"my_app\"\npath = \"src/main.glyph\"\n",
    );
    write_file(
        &app_dir.join("src/main.glyph"),
        "from my_lib import answer as first\nfrom my_lib/api import answer as second\nimport my_lib\nfn helper() -> i32 { ret 1 }\nfn main() -> i32 { let f: FnOnce<(), i32> = my_lib::answer\nret helper() + first() + second() + f() }\n",
    );

    cargo_bin_cmd!("glyph")
        .current_dir(&app_dir)
        .arg("build")
        .assert()
        .success();
    let output = Command::new(app_dir.join("target/debug/my_app"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
}
