use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use glyph_frontend::{FrontendOptions, compile_source};

#[derive(Parser, Debug)]
#[command(name = "glyphfmt", about = "Minimal formatter placeholder")]
struct Args {
    /// File to format
    path: PathBuf,
}

fn format_source(source: &str) -> Result<&str> {
    // glyphfmt is deliberately syntax-preserving until the AST printer lands.
    // Compiling here ensures new syntax is never silently damaged or emitted
    // when it is invalid.
    let output = compile_source(source, FrontendOptions::default());
    if output.diagnostics.is_empty() {
        Ok(source)
    } else {
        for diag in output.diagnostics {
            eprintln!("{:?}", diag);
        }
        anyhow::bail!("format failed: parse errors")
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let source = fs::read_to_string(&args.path)?;

    print!("{}", format_source(&source)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_golden_is_preserved_exactly_and_idempotently() {
        let source = r#"fn main() -> i32 {
  let zero: FnOnce<(), i32> = () -> 1
  let one: FnOnce<i32, i32> = value -> value + 1
  let many: FnOnce<(i32, i32), i32> = (left, right) -> {
    left + right
  }
  ret zero() + one(1) + many(1, 1)
}
"#;

        let once = format_source(source).expect("closure source should format");
        let twice = format_source(once).expect("formatted closure source should format again");
        assert_eq!(once, source);
        assert_eq!(twice, source);
    }

    #[test]
    fn move_and_nested_closure_golden_is_preserved() {
        let source = r#"fn main() -> i32 {
  let base: i32 = 40
  let outer: FnOnce<i32, FnOnce<i32, i32>> =
    move (left: i32) -> (right: i32) -> base + left + right
  let inner = outer(1)
  ret inner(1)
}
"#;

        assert_eq!(format_source(source).unwrap(), source);
    }

    #[test]
    fn malformed_closure_is_not_emitted() {
        let source = "fn main() -> i32 { let bad = x, y -> x + y ret 0 }";
        assert!(format_source(source).is_err());
    }
}
