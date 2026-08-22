use glyph_frontend::{FrontendOptions, compile_source};

#[test]
fn format_i32_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let x: i32 = 42;
  std::print($"The answer is {x}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains fmt_i32 call
    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_i32"), "MIR should call fmt_i32");
}

#[test]
fn format_bool_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let flag: bool = true;
  std::print($"Flag is {flag}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains fmt_bool call
    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_bool"), "MIR should call fmt_bool");
}

#[test]
fn format_u32_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let x: u32 = 4294967295;
  std::print($"u32={x}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_u32"), "MIR should call fmt_u32");
}

#[test]
fn format_i64_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let x: i64 = 922337203685477580;
  std::print($"i64={x}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_i64"), "MIR should call fmt_i64");
}

#[test]
fn format_u64_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let x: u64 = 1844674407370955161;
  std::print($"u64={x}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_u64"), "MIR should call fmt_u64");
}

#[test]
fn format_char_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let ch: char = 'Z';
  std::println($"char={ch}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_char"), "MIR should call fmt_char");
}

#[test]
fn format_str_in_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  let name: str = "world";
  std::print($"Hello {name}!");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains fmt_str call
    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_str"), "MIR should call fmt_str");
}

#[test]
fn format_mixed_types() {
    let source = r#"import std

fn main() -> i32 {
  let x: i32 = 42;
  let flag: bool = true;
  let name: str = "test";
  std::println($"x={x}, flag={flag}, name={name}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains all fmt calls
    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_i32"), "MIR should call fmt_i32");
    assert!(mir_text.contains("glyph_fmt_write_bool"), "MIR should call fmt_bool");
    assert!(mir_text.contains("glyph_fmt_write_str"), "MIR should call fmt_str");
}

#[test]
fn format_literal_segments() {
    let source = r#"import std

fn main() -> i32 {
  let x: i32 = 100;
  std::print($"Result: {x} units");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR uses raw_write for literal segments
    let mir_text = format!("{:?}", output.mir);
    assert!(
        mir_text.contains("raw_write"),
        "MIR should use raw_write for literals"
    );
    assert!(mir_text.contains("glyph_fmt_write_i32"), "MIR should call fmt_i32");
}

#[test]
fn println_adds_newline() {
    let source = r#"import std

fn main() -> i32 {
  std::println($"test");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR includes newline literal
    let mir_text = format!("{:?}", output.mir);
    // The newline should be written as a separate raw_write call
    assert!(
        mir_text.contains("\\n"),
        "MIR should include newline for println"
    );
}

#[test]
fn eprint_uses_stderr() {
    let source = r#"import std

fn main() -> i32 {
  std::eprint($"error");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR creates Stderr struct (fd=2)
    let mir_text = format!("{:?}", output.mir);
    assert!(
        mir_text.contains("Stderr"),
        "MIR should use Stderr for eprint"
    );
}

#[test]
fn format_inline_literal_i32() {
    let source = r#"import std

fn main() -> i32 {
  std::print($"The answer is {42}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains fmt_i32 call for literal
    let mir_text = format!("{:?}", output.mir);
    assert!(
        mir_text.contains("glyph_fmt_write_i32"),
        "MIR should call fmt_i32 for inline literal"
    );
}

#[test]
fn format_inline_literal_bool() {
    let source = r#"import std

fn main() -> i32 {
  std::print($"Flag: {true}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR contains fmt_bool call
    let mir_text = format!("{:?}", output.mir);
    assert!(
        mir_text.contains("glyph_fmt_write_bool"),
        "MIR should call fmt_bool for inline literal"
    );
}

#[test]
fn format_multiple_holes() {
    let source = r#"import std

fn main() -> i32 {
  std::println($"{1} {2} {3}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Verify MIR has multiple fmt_i32 calls
    let mir_text = format!("{:?}", output.mir);
    let count = mir_text.matches("glyph_fmt_write_i32").count();
    assert!(
        count >= 3,
        "MIR should have at least 3 fmt_i32 calls, found {}",
        count
    );
}

#[test]
fn format_empty_interpolation() {
    let source = r#"import std

fn main() -> i32 {
  std::print($"");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Should compile successfully even with empty string
}

#[test]
fn format_only_holes_no_literals() {
    let source = r#"import std

fn main() -> i32 {
  let x: i32 = 42;
  std::print($"{x}");
  ret 0
}
"#;

    let output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        output.diagnostics.is_empty(),
        "diagnostics: {:?}",
        output.diagnostics
    );

    // Should work with only holes, no literal segments
    let mir_text = format!("{:?}", output.mir);
    assert!(mir_text.contains("glyph_fmt_write_i32"), "MIR should call fmt_i32");
}
