```
 ██████╗ ██╗  ██╗   ██╗██████╗ ██╗  ██╗
██╔════╝ ██║  ╚██╗ ██╔╝██╔══██╗██║  ██║
██║  ███╗██║   ╚████╔╝ ██████╔╝███████║
██║   ██║██║    ╚██╔╝  ██╔═══╝ ██╔══██║
╚██████╔╝███████╗██║   ██║     ██║  ██║
 ╚═════╝ ╚══════╝╚═╝   ╚═╝     ╚═╝  ╚═╝
```

**The programming language designed for the LLM era.**

[![CI](https://github.com/glyph-lang/glyph/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/glyph-lang/glyph/actions/workflows/ci.yml) ![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue) ![Rust Edition 2024](https://img.shields.io/badge/rust-edition%202024-orange)

Glyph is an experimental compiled systems language with a **small, predictable syntax** that LLMs can generate correctly and humans can read effortlessly. Powered by Rust and LLVM, it produces native binaries with zero runtime overhead.

> **Status:** Early prototype. The CLI and language are evolving quickly. This is a research project pushing the boundaries of LLM-oriented language design.

---

## ⚡ Why Glyph?

Traditional languages were designed for human programmers typing at keyboards. Glyph is rethinking this for an era where **AI assistants generate much of our code**.

**The Design Principles:**

- **Small keyword set** — Just 24 keywords total (`fn`, `let`, `mut`, `ret`, `if`, `else`, `for`, `in`, `while`, `match`, `struct`, `enum`, `impl`, `interface`, `use`, `import`, `from`, `as`, `pub`, `type`, `const`, `break`, `cont`, `extern`). Fewer concepts for LLMs to get wrong.

- **Regular, deterministic grammar** — No macros, no context-dependent parsing, no special cases. One obvious way to do things.

- **Terse but readable** — `fn` not `function`, `ret` not `return`, semicolons optional. Short enough to scan quickly, clear enough to understand.

- **Predictable type system** — Strong static typing with inference for locals. No hidden behavior.

- **Native performance** — LLVM backend producing real executables. No garbage collector, no runtime overhead.

**The result?** A language where LLMs make fewer syntax errors, generate more consistent code, and the output remains readable to humans.

---

## 🚀 See It In Action

**Hello World** — clean and simple:

```glyph
from std import println

fn main() -> i32 {
  println("hello world")
  ret 0
}
```

**Working with generics and collections:**

```glyph
from std/vec import Vec
from std import println

fn main() -> i32 {
  let v: Vec<i32> = Vec::with_capacity(2)
  v.push(10)
  v.push(20)
  v.push(30)

  let third: i32 = v[2]
  println($"Element at index 2: {third}")
  ret third
}
```

**Pattern matching with match expressions:**

```glyph
fn classify(opt: Option<i32>) -> &str {
  match opt {
    Some(n) => {
      if n < 0 { ret "negative" }
      if n < 10 { ret "small" }
      ret "large"
    },
    None => ret "empty",
  }
}

fn handle_result(res: Result<File, Error>) -> i32 {
  match res {
    Ok(file) => {
      let _ = file.write_string("success")
      0
    },
    Err(e) => 1,
  }
}
```

**Error handling with ? operator:**

```glyph
fn read_file(path: &str) -> Result<String, Error> {
  let f = open(path)?
  let s = f.read()?
  ret Ok(s)
}
```

**Structs and methods:**

```glyph
struct Point {
  x: f64
  y: f64
}

fn distance(p: &Point) -> f64 {
  ret sqrt(p.x * p.x + p.y * p.y)
}
```

---

## ✨ What Works Today

This is not the final language spec, but these features are working now (see `examples/` and `tests/fixtures/`):

**Core Language**
- Function declarations with `fn`, local variables with `let`/`let mut`
- Control flow: `if`/`else`, `while`, `for`/`for-in`, `match` (expression-oriented)
- Return statements with `ret`
- Primitive types: `i32`, `u32`, `i64`, `u8`, `bool`, `f64`, `char`
- Operators: `+`, `-`, `*`, `/`, `%` (modulo), comparisons, logical, `?` (error propagation)
- String interpolation: `$"x = {x}"`, hex escape sequences (`"\x1B[31m"`)

**Type System**
- Generics with monomorphization: `Vec<T>`, `Map<K,V>`, `Option<T>`, `Result<T,E>`
- Structs (product types) and enums (sum types with payloads)
- Type inference for local variables
- Ownership and borrowing: `&T` (shared), `&mut T` (exclusive)
- Trait-like interfaces for polymorphism

**Collections & I/O**
- Generic collections: `Vec<T>`, `Map<K,V>`
- String support: string literals, `&str` slices, owned `String`
- File I/O via `std::io::File`
- JSON parsing via `std/json/parser`
- Command-line arguments via `std::sys::argv`
- Terminal I/O via `std/term` (raw mode, echo control, cursor visibility)
- Standard output: `std::print`, `std::println` with formatting

**Cases and Dependencies**
- Project manifest via `glyph.toml` with `[lib]`, `[[bin]]`, and `[dependencies]`
- Local path-based dependencies between cases
- Transitive dependency resolution with cycle detection
- Library and binary targets

**Advanced Features**
- Multi-file projects with module discovery
- C FFI via `extern "C"` declarations
- Owned, first-class `FnOnce` closures and named-function values
- Native threads, atomic scalars, `Arc<T>`, and worker-only `Mutex<T>` shared state
- Error propagation with `?` operator
- Arrays with bounds checking
- LSP server with real-time diagnostics

---

## 📦 Quickstart

### Prerequisites

- **Rust** (workspace uses Rust edition 2024)
- **LLVM** (used via `llvm-sys` for code generation)

On macOS with Homebrew:

```bash
export LLVM_SYS_201_PREFIX="/opt/homebrew/opt/llvm"
```

### Build the Toolchain

```bash
cargo build --release
```

### Install to System Path (Recommended)

With [just](https://github.com/casey/just) installed:

```bash
# Install the glyph project tool
just install

# Or install all CLI tools
just install-all
```

Without just:

```bash
# Install the glyph project tool
cargo install --path crates/glyph-cli --bin glyph

# Or install all tools
cargo install --path crates/glyph-cli
```

This installs binaries to `~/.cargo/bin/`, which should be in your PATH.

### Try It Out

```bash
# Run the demo (shows source + generated LLVM IR)
./demo.sh

# Check a file (parse + type check)
glyph-cli check examples/std_hello/hello.glyph

# Build and run
glyph-cli run examples/std_hello/hello.glyph

# Build a native executable
glyph-cli build examples/std_hello/hello.glyph --emit exe
./hello
```

---

## 📚 Documentation

The mdBook documentation lives in `docs/book/`. Read the generated book at
https://glyph-lang.github.io/glyph/book/. Build it with:

```bash
mdbook build docs/book
```

**Design documents** in `docs/design/` cover:
- `LLMS.md` — Token efficiency and LLM-oriented design principles
- `GENERICS.md` — Generic type system implementation
- `VEC_DESIGN.md` — Generic vector (`Vec<T>`) design
- `MAP.md` — Associative container (`Map<K,V>`) design
- `INTERFACES_DESIGN.md` — Trait/interface system
- Plus many more covering enums, ownership, networking, formatting, etc.

---

## 🏗️ Project Workflow

### Initialize a New Project

Create a new Glyph project with a single command:

```bash
glyph init my-project
cd my-project
glyph build
glyph run
```

This creates a `glyph.toml` manifest and a `src/main.glyph` file with a basic "Hello, Glyph!" program.

### Manual Setup

Alternatively, create a `glyph.toml` manually:

```toml
[package]
name = "hello"
version = "0.1.0"

[[bin]]
name = "hello"
path = "hello.glyph"
```

Then use the `glyph` executable:

```bash
glyph build --release
glyph run --bin hello -- arg1 arg2
```

### Cases and Dependencies

Glyph projects are organized into **cases** — the unit of compilation. A case can be a binary, a library, or both.

**Library case** (`my_lib/glyph.toml`):

```toml
[package]
name = "my_lib"
version = "0.1.0"

[lib]
path = "src/lib.glyph"
```

**Binary case depending on a library** (`my_app/glyph.toml`):

```toml
[package]
name = "my_app"
version = "0.1.0"

[dependencies]
my_lib = { path = "../my_lib" }

[[bin]]
name = "my_app"
path = "src/main.glyph"
```

Import from a dependency using its case name:

```glyph
from my_lib import greet
from my_lib/helpers import format_name
```

See the [Cases and Dependencies](https://glyph-lang.github.io/glyph/book/cases.html) chapter in the book for the full guide.

---

## 🔧 Compiler Architecture

The repo contains a complete working compiler pipeline:

**Frontend** (`crates/glyph-frontend`)
- Lexer + parser (using `nom` parser combinators)
- Multi-module resolution and symbol resolution
- Type inference and checking
- Generic monomorphization
- MIR lowering (SSA-ish intermediate representation)

**Backend** (`crates/glyph-backend`)
- LLVM IR code generation
- Native object emission and linking
- Integration with system libraries

**Core** (`crates/glyph-core`)
- Shared AST, MIR, and type system definitions (`ast`, `mir`, `types`, `token`, `span`)
- Diagnostic and error reporting infrastructure

**Runtime** (`runtime/`)
- Minimal C runtime for formatting and I/O
- Linked with generated binaries

**CLI Tools** (`crates/glyph-cli`)
- `glyph-cli` — Direct compiler driver (check/build/run)
- `glyph` — Project workflow tool (build via `glyph.toml`)

**Tooling**
- `glyphlsp` — Language server providing real-time diagnostics
- `glyphfmt` — Syntax-preserving formatter frontend

---

## 🧪 Development

Run the full test suite:

```bash
cargo test
```

Frontend-only tests (no LLVM required):

```bash
cargo test -p glyph-frontend
```

---

## 🗂️ Repository Layout

```
crates/
  glyph-frontend/   # Lexer, parser, resolver, MIR lowering
  glyph-backend/    # LLVM codegen and linking
  glyph-core/       # Shared AST/MIR/types and diagnostics
  glyph-cli/        # CLI binaries (glyph, glyph-cli)
  glyphlsp/         # Language server (real-time diagnostics)
  glyphfmt/         # Syntax-preserving formatter frontend

runtime/            # Minimal C runtime for I/O
examples/           # Working end-to-end programs
tests/fixtures/     # Language and codegen test fixtures
docs/               # mdBook documentation and design docs
```

---

## 🤝 Contributing

This is an experimental research project! Issues and PRs are welcome, especially around:

- **Type checking and diagnostics** — Making error messages even better
- **Stdlib expansion** — Adding more useful standard library functions
- **CLI and build workflow** — Improving the developer experience
- **Formatter and LSP** — Turning placeholders into real tools
- **Language design** — Proposing improvements to syntax or semantics

---

## 📖 Philosophy

###  Glyph Optimizes

1. **LLM correctness** — Simpler grammar means fewer syntax errors when AI generates code
2. **Meta-instruction efficiency** — When you prompt "write Glyph code to do X", the language rules themselves are simpler to explain
3. **Consistency** — One obvious way to do things reduces the decision space for LLMs
4. **Learnability** — Smaller surface area means faster learning for both humans and AI
5. **LLM reasoning efficiency** — Validated through rigorous experimentation (see below)

### Experimental Validation

We ran a study comparing LLM token costs across 8 languages (Python, Clojure, Rust, Glyph, Go, C++, Java, C) with 2 OpenAI models and 7 programming tasks. Full results and all generated code are available:

- **[LLM Token Cost Report](https://glyph-lang.github.io/glyph/llm-token-cost.html)** — interactive results page
- **[Raw data and code](experiments/llm-reasoning-cost/)** — runner script, prompts, and generated code for all languages

---

## 📄 License

Dual-licensed under **MIT OR Apache-2.0** (see workspace metadata in `Cargo.toml`).

---

**Built with Rust 🦀 | Powered by LLVM ⚡ | Designed for AI 🤖**
