# Glyph Language Guide for LLMs

Glyph is a small, expression-oriented compiled language designed for predictable LLM output. This guide summarizes the current language surface and highlights pitfalls that frequently trip up generated code.

For the authoritative, compiler-verified list of value-category, inference, and ABI limits (with quoted diagnostics for what does and doesn't compile), see [`docs/book/src/limitations.md`](docs/book/src/limitations.md).

## Quick Start

```glyph
from std import println

fn main() -> i32 {
  let name = String::from_str("Glyph")
  println($"hello {name}")
  ret 0
}
```

## Core Syntax

Functions and locals:

```glyph
fn add(a: i32, b: i32) -> i32 {
  a + b
}

fn main() -> i32 {
  let mut x = 0
  x = x + 1
  ret x
}
```

- `fn` defines a function.
- `let` binds an immutable local; `let mut` makes it mutable.
- `ret expr` returns early; otherwise the last expression in a block is the value.
- Semicolons are optional. The parser consumes them if present.

### Operators

| Precedence | Operators | Description |
|-----------|-----------|-------------|
| 1 (lowest) | `\|\|` | Logical OR |
| 2 | `&&` | Logical AND |
| 3 | `==`, `!=` | Equality |
| 4 | `<`, `<=`, `>`, `>=` | Comparison |
| 5 | `+`, `-` | Addition, subtraction |
| 6 | `*`, `/`, `%` | Multiplication, division, modulo (integer remainder via LLVM `srem`) |
| 7 (highest) | `!`, `-` (unary) | Logical NOT, negation |

The `?` operator works on `Result` types for error propagation: `let val = expr?`.

Comments:

- Line comments: `// ...`
- Block comments: `/* ... */`

## Control Flow

`if` and `match` are expressions:

```glyph
fn abs_i32(x: i32) -> i32 {
  if x < 0 { -x } else { x }
}

fn classify(opt: Option<i32>) -> i32 {
  let v = match opt {
    Some(n) => n,
    None => 0,
  }
  ret v
}
```

Loops:

```glyph
fn sum_to(n: i32) -> i32 {
  let mut acc = 0
  for i in 0..n {
    acc = acc + i
  }
  ret acc
}
```

- `for i in start..end` is end-exclusive (`i < end`).
- `for x in collection { ... }` iterates over a collection (e.g. `Vec<T>`).
- Use `break` and `cont` (not `continue`).
- In `for`/`for-in`, `cont` still runs the loop increment step before the next iteration.

Void functions (no `-> Type` annotation):

```glyph
fn greet(name: str) {
  println($"hello {name}")
}

fn main() -> i32 {
  greet("world")
  ret 0
}
```

Void calls can appear anywhere in a block, not just at the end.

## Types and Literals

Common built-ins:

- Integers: `i8`, `i32`, `i64`, `u8`, `u32`, `u64`, `usize` (integer literals default to `i32`).
- Floats: `f32`, `f64` (limited support; see pitfalls).
- `bool`, `char`, `str` (borrowed), `String` (owned).

Strings:

- String literals like `"hi"` are `str`.
- Use `String::from_str("...")` to allocate an owned `String`.
- Use `s.clone()` to duplicate a `String` without moving it.
- Hex escape sequences are supported: `"\x1B[31m"` embeds byte value `0x1B` (ESC). Format: `\xHH` where HH is two hex digits.

Generics:

- `Vec<T>`, `Map<K, V>`, `Option<T>`, `Result<T, E>` live in `std/vec`, `std/map`, `std/enums`.

## Ownership and Pointers

```glyph
fn main() -> i32 {
  let heap = Own::new(42)
  let raw = heap.into_raw()
  let back = Own::from_raw(raw)
  let _keep_alive = back
  ret 0
}
```

- `&T` / `&mut T` are borrowed references.
- `Own<T>` is single-owner heap allocation.
- `Shared<T>` is ref-counted shared ownership.
- `RawPtr<T>` is unsafe; only use for FFI boundaries.

## Collections

Vectors (`Vec<T>`):

```glyph
from std/vec import Vec

fn main() -> i32 {
  let mut v: Vec<i32> = Vec::new()
  v.push(1)
  let _p = v.pop()
  let _len = v.len()
  let _x = v[0]
  ret 0
}
```

Maps (`Map<K, V>`):

```glyph
from std/map import Map

fn main() -> i32 {
  let m: Map<i32, i32> = Map::new()
  let _ = m.add(1, 2)
  let _ = m.update(1, 3)
  let _ = m.get(1)
  ret 0
}
```

Maps with owned keys/values (e.g. `Map<String, String>`) are fully supported — drop glue walks all buckets and frees keys, values, and nodes automatically.

Arrays:

```glyph
fn main() -> i32 {
  let a = [1, 2, 3]
  let x = a[0]
  let len = a.len()
  ret x + len
}
```

- Array literals must be non-empty.

## I/O and Processes

File I/O:

```glyph
from std/io import File

fn main() -> i32 {
  let opened = File::open("out.txt")
  let status = match opened {
    Ok(file) => {
      let _ = file.write_string(String::from_str("hi"))
      let _ = file.close()
      0
    },
    Err(_) => 1,
  }
  ret status
}
```

Processes:

```glyph
from std/process import run
from std/vec import Vec

fn main() -> i32 {
  let mut args: Vec<String> = Vec::new()
  args.push(String::from_str("--version"))
  ret run("git", args)
}
```

## Imports

Imports must appear before any items.

```glyph
from std import println
from std/enums import Option
import std/io

fn main() -> i32 {
  let _ = std::io::File::create("out.txt")
  ret 0
}
```

- Use `/` in module paths; omit the `.glyph` extension.
- `import path` gives qualified access (`std::io::File`).
- `from path import Name` brings symbols into scope.
- `import Name as Alias from path` supports aliases.
- Parent directory imports (`..`) are rejected.

## Printing and Interpolation

`print`/`println` accept string literals, interpolated strings, or runtime `str`/`String` values.
`print_str` prints without a trailing newline:

```glyph
from std/io import print_str

fn main() -> i32 {
  print_str("no newline")
  ret 0
}
```

Examples with `println`:

```glyph
from std import println

fn main() -> i32 {
  let x = 3
  let msg = String::from_str("hello")
  println($"x = {x}")
  println(msg)
  ret 0
}
```

Use `s.as_str()` when you need an explicit borrowed view of a `String`.

## Cases and Dependencies

Glyph projects are organized into **cases** (the unit of compilation). A case has a `glyph.toml` manifest:

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
```

Dependencies are resolved transitively with cycle detection.

## Idiosyncrasies and Pitfalls (LLM Checklist)

- Keywords are short: `ret` (not `return`), `cont` (not `continue`).
- `match` only works on enum values; patterns are `Variant(binding)` or `_`.
- `match` consumes the scrutinee by value — the matched local is moved and should not be used after the `match`.
- `match` must be exhaustive; add `_ => ...` if needed.
- Many std methods require the receiver to be a local variable (not a temporary). Always assign before calling:
  - Good: `let s = String::from_str("hi"); s.len()`
  - Bad: `String::from_str("hi").len()`
- Struct fields are not moveable by value. Borrow (`let x: str = s.field`), clone (`s.field.clone()`), or move the whole struct.
- String interpolation with `{expr}` is supported in `print`/`println`, but not in general expressions.
- Bare `Vec::new()`/`Map::new()` infer their type parameters from later use in the same function (a push/add call, the enclosing `let`'s annotation, or the function's return type); annotate for readability or when nothing else constrains the type.
- Assignment targets can be identifiers, struct/tuple fields, or an index expression (`xs[i] = v`).
- References can only be taken to locals (`&local`), not to temporaries.
- Array `.len()` only works on local array variables.
- The `?` operator works on `Result` types for error propagation (e.g. `let val = expr?`). The enclosing function must return `Result`.
- Enum variants with owned-type payloads (e.g. `Option<String>`, `Result<String, E>`) are properly drop-managed — the active variant's payload is freed automatically when the enum goes out of scope.
- `break` and `cont` in loops properly drop locals scoped inside the loop body.
- `for`/`for-in` `cont` jumps to the loop's continue target (increment/index advance), not directly to condition check.
- Floats (`f64`) are fully supported: literals, arithmetic, comparisons, int/float promotion, params/returns, `Vec<f64>`, and `as` casts. The one open gap is a negative literal assigned to a float `let` (`let x: f64 = -1`), which is not promoted correctly - see `docs/book/src/limitations.md`.
- A bare `if`/`else` used as a function body's last statement (no explicit `ret`) does not return the taken branch's value - see `docs/book/src/limitations.md`. Always write `ret if ... { ... } else { ... }` explicitly; `match` in the same tail position works correctly.
- `extern` functions must end with `;` and only `extern "C"` is accepted.
- `const` declarations require an explicit type annotation.

## LLM-Friendly Patterns

- Prefer explicit types for generics and `Vec::new()`/`Map::new()`.
- Use locals for receivers of std methods (File/String/Vec/Map).
- Use `println($"...{expr}...")` to print non-string values.
- Keep control flow expression-oriented: assign `if`/`match` results to locals.
