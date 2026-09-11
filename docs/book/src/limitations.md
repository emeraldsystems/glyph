# Limitations

Glyph's type system and codegen have real, sharp-edged limits. This page is
the authoritative list of value-category, inference, and ABI rules that are
part of the current release contract, not scattered caveats. Every rule below
was verified against the current compiler by compiling a small program; the
exact diagnostic is quoted where one is produced. For a broader pass/fail
matrix across language features, see `docs/release/validation-matrix.md`.

## Collection constructors

Bare `Vec::new()` / `Map::new()` infer their type parameters from later use in
the same function (a `push`/`add` call, the enclosing `let`'s annotation, or
the function's declared return type) - an explicit annotation is not
required.

**Supported**

```glyph
fn main() -> i32 {
  let mut v = Vec::new()
  v.push(String::from_str("a"))
  v.push(String::from_str("bb"))
  ret v.len() as i32
}
```

```glyph
fn make() -> Vec<String> {
  ret Vec::new()  // element type inferred from the return type
}
```

An annotation is still useful for readability, and is required if nothing in
the function ever constrains the type parameter (in that case the compiler
falls back to a default rather than erroring).

## Local-receiver requirement and struct collection fields

A struct field of collection or array type (`Vec<T>`, `Map<K,V>`, `[T; N]`)
cannot have a method called on it, be indexed, or be moved out of the struct
directly. `glyph-cli check` (type-check only) does not catch this - only
`glyph-cli build` (and `glyph run`) does, because the check happens during MIR
lowering, later than type-checking.

**Not supported** (diagnostic: `cannot move field 'items' out of a struct;
borrow it (...), clone it (...), or move the whole struct`)

```glyph
struct Holder { items: Vec<i32> }

fn main() -> i32 {
  let mut h = Holder { items: Vec::new() }
  h.items.push(1)          // error, and so does h.items.len(), h.items.clone()
  ret h.items.len() as i32
}
```

**Supported** - borrow the field into a function parameter:

```glyph
struct Holder { items: Vec<i32> }

fn push_one(v: &mut Vec<i32>) -> i32 {
  v.push(1)
  ret v.len() as i32
}

fn main() -> i32 {
  let mut h = Holder { items: Vec::new() }
  ret push_one(&mut h.items)   // 1
}
```

This also affects fixed-size array fields (`[T; N]`) - see
[Array-typed struct fields](#array-typed-struct-fields-glyph-79) below.

## Cross-module field access

Accessing a struct's fields from another module requires importing the struct
**type** itself, not just the functions that construct or return it.

**Not supported** (diagnostic: `struct 'Point' has no field named 'x'`)

```glyph
// main.glyph
from point import make_point   // Point itself is not imported

fn main() -> i32 {
  let p = make_point(3, 4)
  ret p.x + p.y               // error
}
```

**Supported**

```glyph
from point import make_point, Point

fn main() -> i32 {
  let p = make_point(3, 4)
  ret p.x + p.y                // 7
}
```

## Match: consumption, exhaustiveness, patterns

`match` moves (consumes) its scrutinee by value; the matched local cannot be
used again afterward.

**Not supported** (diagnostic: `` `use of moved value `o` of type `Option<i32>`` ``)

```glyph
from std/enums import Option

fn main() -> i32 {
  let o = Option::Some(5)
  let r = match o { Some(v) => v, None => 0 }
  let r2 = match o { Some(v) => v, None => 0 }  // error: o already moved
  ret r + r2
}
```

`match` must be exhaustive.

**Not supported** (diagnostic: `` `non-exhaustive match: cover all variants or
add `_` arm` ``)

```glyph
from std/enums import Option

fn main() -> i32 {
  let o = Option::Some(5)
  ret match o { Some(v) => v }   // missing None
}
```

Integer-literal patterns are not supported (GLYPH-66) - use `if`/`else`
instead.

**Not supported** (diagnostic: `expected variant name`)

```glyph
fn classify(n: i32) -> i32 {
  ret match n { 0 => 100, 1 => 200, _ => 300 }   // error
}
```

**Supported**

```glyph
fn classify(n: i32) -> i32 {
  if n == 0 { 100 } else if n == 1 { 200 } else { 300 }
}
```

Bare unit enum variants construct and match without parentheses.

**Supported**

```glyph
enum Val { Nil, Num(i32) }

fn main() -> i32 {
  let v = Val::Nil
  ret match v { Nil => 0, Num(_n) => 1 }   // 0
}
```

## References are local-only

`&T` / `&mut T` can only be taken to a local variable, cannot be stored in a
struct field, and cannot be returned from a function.

**Not supported** (diagnostic: `a borrowed reference or callable cannot be
stored in an aggregate; keep it in a direct lexical binding`)

```glyph
struct Holder { r: &i32 }

fn main() -> i32 {
  let x = 5
  let h = Holder { r: &x }   // error
  ret 0
}
```

**Not supported** (diagnostic: `borrowed references cannot escape through
return; return owned data instead`)

```glyph
fn get_ref(x: &i32) -> &i32 {
  ret x   // error
}
```

Field access through a reference does auto-dereference:

**Supported**

```glyph
struct Point { x: i32 }

fn get_x(p: &Point) -> i32 {
  ret p.x
}

fn main() -> i32 {
  let point = Point { x: 4 }
  ret get_x(&point)   // 4 - note &point, not &Point{x:4}: references
                       // can only be taken to locals, not temporaries
}
```

**Known issue (untracked, found while verifying this page):** binary
operators do **not** auto-dereference a `&i32` the way field access does.
`ret x + 1` where `x: &i32` incorrectly reports `scoped or borrowed value
cannot escape through return` (a misleading diagnostic - the result is a
plain `i32`, not a reference). Worse, if the same expression is not directly
returned (e.g. `let v = x; let sum = v + 1; ret 0`), the compiler emits
invalid LLVM IR and crashes with `LLVM module verification failed: Both
operands to a binary operator are not of the same type! %binop = add ptr
%load1, i32 1` instead of producing any diagnostic. The same applies to
comparisons (`x == 5`). This reproduces on current master and should be
filed as a new bug; workaround: copy the referenced value into a local via
a struct/function boundary that already dereferences it (e.g. field access),
rather than doing arithmetic directly on a `&i32` parameter.

## Array literals

Fixed-size arrays (`[T; N]`) work, and the element type follows the
annotation.

**Supported**

```glyph
fn main() -> i32 {
  let a: [i32; 3] = [1, 2, 3]
  ret a[0] + a[1] + a[2]   // 6
}
```

An element literal that is out of range for the declared element type is now
a compile error (GLYPH-74).

**Not supported** (diagnostic: `integer literal 300 is out of range for 'u8'
(valid range 0..=255)`)

```glyph
fn main() -> i32 {
  let a: [u8; 2] = [300, 1]   // error
  ret a[0] as i32
}
```

**Known issue (untracked):** a literal with **more elements than the
declared size** is not rejected - `let a: [i32; 2] = [1, 2, 3]` compiles and
runs without any diagnostic (indexing only the first two slots). This looks
like a gap in the same check that GLYPH-74 added for out-of-range values, and
should be filed as a follow-up.

### Array-typed struct fields (GLYPH-79)

Still open. Both moving an array field out of a struct and indexing into it
directly - even through a `&Struct` parameter - hit the same "cannot move
field out of a struct" diagnostic as collection fields (see
[Local-receiver requirement](#local-receiver-requirement-and-struct-collection-fields)
above); there is currently no working pattern to read an individual element
of an array-typed struct field.

**Not supported** (diagnostic: `cannot move field 'xs' out of a struct;
borrow it (...), clone it (...), or move the whole struct`)

```glyph
struct Holder { xs: [i32; 3] }

fn get(h: &Holder) -> i32 {
  ret h.xs[1]   // error, even through a reference
}
```

### Negative literal into a float type (GLYPH-78)

Still open, and worse than "not promoted": it is a silent wrong-value bug,
not a rejection.

**Not supported (silently wrong, no diagnostic)**

```glyph
from std/io import println

fn main() -> i32 {
  let x: f64 = -1
  println($"{x}")   // prints 4.2439915814e-314, not -1
  ret 0
}
```

### Large literal as an inline `as` operand (GLYPH-75)

Still open: a literal that doesn't fit `i32` is truncated to `i32` range
*before* the `as` cast is applied, when it appears directly as the cast's
operand.

**Not supported (silently wrong, no diagnostic)**

```glyph
from std/io import println

fn main() -> i32 {
  let x = 6300000000 as u64
  println($"{x}")   // prints 2005032704 (6300000000 mod 2^32), not 6300000000
  ret 0
}
```

Workaround: bind the value to an explicitly `u64`-typed `let` first, or use a
literal that already fits `i32` before widening.

## Integers

Widths: `i8`/`i16`/`i32`/`i64`, `u8`/`u16`/`u32`/`u64`, `usize`. There are
**no shift operators**.

**Not supported** (diagnostic: `unexpected token in expression`)

```glyph
fn main() -> i32 {
  ret 1 << 3   // error - not part of the grammar
}
```

Arithmetic wraps at each width's boundary.

**Supported**

```glyph
from std/io import println

fn main() -> i32 {
  let x: i8 = 127
  println($"{x + 1}")   // -128
  ret 0
}
```

Implicit **lossless widening** at call/let/field/return boundaries is
accepted, and an unsigned source zero-extends.

**Supported**

```glyph
fn take_i64(x: i64) -> i64 { ret x }

fn main() -> i32 {
  let a: i32 = 42
  ret take_i64(a) as i32   // 42, i32 -> i64 widens implicitly
}
```

Unsigned `<`, `/`, and `%` use unsigned semantics (GLYPH-76).

**Supported**

```glyph
fn main() -> i32 {
  let a: u32 = 4294967295   // u32::MAX
  let b: u32 = 2
  ret if a < b { 1 } else { 0 }   // 0 - correctly unsigned, not -1 < 2
}
```

`Vec<u8>` works at any size (GLYPH-72; previously corrupted the heap above a
threshold).

**Supported** - pushing 5000 `u8` elements works and reports the correct
length.

**Known issue, decision pending (GLYPH-77):** implicit **narrowing** (e.g.
`i64` -> `i32` parameter) silently truncates, and a same-width signed <->
unsigned conversion is silently accepted as a bit-reinterpretation. Neither
produces a diagnostic today; an explicit `as` is recommended at these
boundaries until GLYPH-77 is decided.

**Currently silent (no diagnostic)**

```glyph
fn take_i32(x: i32) -> i32 { ret x }

fn main() -> i32 {
  let big: i64 = 4294967297     // 2^32 + 1
  ret take_i32(big)             // 1, truncated silently
}
```

```glyph
from std/io import println

fn take_u32(x: u32) -> u32 { ret x }

fn main() -> i32 {
  let s: i32 = -1
  println($"{take_u32(s)}")   // 4294967295 (u32::MAX) - bits reinterpreted
  ret 0
}
```

## Floats

Fully supported: literals, arithmetic, comparisons, int/float promotion,
`f64` params/returns, `Vec<f64>`, `as` casts with Rust semantics, and
`std/math`.

**Supported**

```glyph
from std/vec import Vec

fn add(a: f64, b: f64) -> f64 { ret a + b }

fn main() -> i32 {
  let z = add(1.5, 2.5)
  let mut v: Vec<f64> = Vec::new()
  v.push(z)
  ret if v[0] > 3.0 { 0 } else { 1 }   // 0
}
```

See [Negative literal into a float type](#negative-literal-into-a-float-type-glyph-78)
above for the one open gap (negative literals specifically).

## `extern "C"` ABI

Scalars (`i8`..`i64`, `u8`..`u64`, `f32`/`f64`, `bool`) and `str` (as
`char*`) cross the FFI boundary correctly.

**Supported**

```glyph
extern "C" fn sqrt(x: f64) -> f64;
extern "C" fn strlen(s: str) -> i64;

fn main() -> i32 {
  let r = sqrt(9.0) as i32     // 3
  let n = strlen("hello")       // 5
  ret r
}
```

Droppable/owned Glyph types (`Vec<T>`, structs, `String`) cannot be passed
**by value**; pass a reference instead.

**Not supported** (diagnostic: `extern function 'some_fn' parameter 'v'
cannot take ownership of Glyph droppable type 'Vec<i32>' by value; pass a
reference or an ABI-safe scalar/RawPtr instead`)

```glyph
from std/vec import Vec

extern "C" fn some_fn(v: Vec<i32>) -> i32;   // error
```

**Supported** - a struct passed by reference:

```glyph
struct Point { x: i32, y: i32 }

extern "C" fn some_fn(p: &Point) -> i32;   // fine
```

Library linking is configured via `glyph.toml`'s `[link]` section
(`libs`/`search_paths`), which plumbs through to the linker.

## Strings

`str` is a borrowed slice; `String` is owned (heap-allocated). Coercing an
owned `String` to `str` **moves** it - the original `String` local can no
longer be used afterward.

**Not supported** (diagnostic: `` `use of moved value `owned` of type
`String`` ``)

```glyph
fn main() -> i32 {
  let owned = String::from_str("hi")
  let sr: str = owned      // moves `owned`
  ret owned.len() as i32   // error - use `owned.clone()` to keep both
}
```

The other direction copies. A `str` view (a string literal, a `str` binding
or parameter) that lands in a `String` slot - a `String`-typed `let`, a
`String` return value (`fn f() -> String { "x" }`), a `String` parameter, a
`Vec<String>` push, an `Option<String>` / `Result<String, _>` payload - is
heap-copied at that point, exactly once, and the resulting `String` is owned
and dropped like any other (GLYPH-87, fixed). Comparing strings with `==` /
`!=` reads both operands and moves neither, so a `String` can be compared and
then used or dropped normally.

**Supported**

```glyph
fn label(n: i32) -> String {
  if n == 0 { "zero" } else { "other" }   // each literal is copied once
}

fn main() -> i32 {
  if label(0) != "zero" { ret 1 }         // the temporary is freed once
  let s = label(1)
  if s != "other" { ret 2 }               // `s` is still owned here
  if s != "other" { ret 3 }
  ret 0
}
```

`print`/`println` only accept a string literal, an interpolated string, or a
`str`/`String` value - not a bare number or other scalar. Interpolate with
`$"{expr}"` to print non-string values.

**Not supported** (diagnostic: `print/println require a string literal,
interpolated string, or str/String value`)

```glyph
from std/io import println

fn main() -> i32 {
  println(5)   // error
  ret 0
}
```

**Supported**

```glyph
from std/io import println

fn main() -> i32 {
  println($"{5}")
  ret 0
}
```

## `ret` is a statement, not an expression

`ret` cannot appear in expression position, such as inside a `match` arm used
as a value.

**Not supported** (diagnostic: `unexpected token in expression`)

```glyph
from std/enums import Result

fn maybe() -> Result<i32, str> {
  let x = match 1 { _ => ret Err("bad") }   // error - ret is not an expression
  ret Ok(x)
}
```

Use match-as-statement with blocks instead:

```glyph
from std/enums import Option, Result

fn maybe(o: Option<i32>) -> Result<i32, str> {
  match o {
    None => { ret Err("bad") },
    Some(_v) => {},
  }
  ret Ok(1)
}
```

`cont` means `continue`; `break` means `break`.

Note: `while true { ... }` no longer needs a trailing unreachable `ret` after
it on the current compiler - a prior version of this guidance said it did.
Both a `while true` with an internal `break` and a truly infinite `while
true` (no `break`) compile as the last statement of a non-void function
without an extra `ret`.

### `if`/`else` as an implicit tail return

Glyph is expression-oriented ("the last expression in a block is the
value"), and this holds for plain expressions, for `match`, and for
`if`/`else` alike when any of them is a function's tail expression (GLYPH-84,
fixed). A bare `if`/`else` occupying a function body's own tail position - no
explicit `ret` needed - returns the taken branch's value, the same as it
already did when bound to a `let` or nested inside a `{ }` block:

```glyph
from std import println

fn pick(n: i32) -> i32 {
  if n == 0 { 100 } else { 300 }   // no `ret` needed
}

fn main() -> i32 {
  println($"{pick(0)}")   // prints 100
  println($"{pick(1)}")   // prints 300
  ret 0
}
```

This requires an `else`: a tail `if` with no `else` has nothing to return in
a non-void function, and is a compile error (`if expression is missing an
else branch`) rather than a silently wrong value. A tail `if` with no `else`
in a `void` function is unaffected - there is no return value to produce
either way.

```glyph
fn pick(n: i32) -> i32 {
  if n == 0 { 100 }   // error: missing an else branch
}
```

## Views and ownership

`Map::get`, `Vec` indexing, and struct field views deep-clone at
ownership-escape points, so taking a nested value out of a `Map` by value
(including a `Map` nested inside another `Map`-held `enum` payload) is safe -
no dangling references or double frees, even under repeated allocation and
drop.

**Supported**

```glyph
from std/enums import Option
from std/map import Map

fn main() -> i32 {
  let mut i: i32 = 0
  let mut failed = 0
  while i < 100 {
    let mut m: Map<String, i32> = Map::new()
    let _ = m.add(String::from_str("k"), 1)
    let got = m.get(String::from_str("k"))
    match got {
      Some(_v) => {},
      None => { failed = 1 },
    }
    i = i + 1
  }
  ret failed   // 0
}
```

## Closures, threads, and Send/Sync

Closures use arrow syntax (`x -> x + 1`, `move`) and are typed as
`FnOnce<Args, Return>`, `Fn<Args, Return>`, or `FnMut<Args, Return>`. See
[Closures and Callable Values](closures.md) for the full syntax.
`std/thread` provides `spawn`/scoped threads and typed `JoinHandle<T>`;
`std/sync` provides `Arc<T>`, `Mutex<T>`, SPSC channels, and atomics.

Calling a value of callable type directly requires an **exact** argument
type match - unlike an ordinary function call, no implicit widening happens
at that call site.

**Not supported** (diagnostic: `argument 1 to 'g' has type 'i32', expected
'i64'`)

```glyph
fn main() -> i32 {
  let g: FnOnce<i64, i64> = a -> a + 1
  let x: i32 = 5
  ret g(x) as i32   // error - x must already be i64
}
```

`Vec`, `Map`, `Option`, and `Result` are `Send` when their element/payload
type is `Send` (GLYPH-63). `Shared<T>` is single-threaded and is never
`Send`, including inside a collection - `Arc<T>` is the thread-safe
alternative.

**Not supported** (diagnostic: `` `spawn task.capture `v`.value` is not
Send: `Shared<T>` is not synchronized and cannot cross threads ``)

```glyph
from std/vec import Vec
import spawn from std/thread

fn main() -> i32 {
  let mut v: Vec<Shared<i32>> = Vec::new()
  v.push(Shared::new(1))
  let task: FnOnce<(), ()> = move () -> { let n = v.len() }
  ret spawn(task)   // error
}
```

**Supported** - `Arc<T>` crossing into a spawned task works.

## Imports

Matching a stdlib `Result` (its `Ok`/`Err` constructors, or using `Result`
as a type) requires importing it explicitly - a module's internal imports do
not propagate to its consumers.

**Not supported** (diagnostic: `unknown function 'Ok'` / `unknown enum type
'Result'`)

```glyph
fn get() -> Result<i32, str> { ret Ok(5) }   // error without the import below
```

**Supported**

```glyph
from std/enums import Result

fn get() -> Result<i32, str> { ret Ok(5) }
```

`import std` before a `from std/x import Y` line no longer misparses
(previously GLYPH-30).

## Compiler-internal diagnostics

A MIR verifier runs before codegen (GLYPH-3). If you see `MIR verification
failed before codegen (N error(s)): ...`, that indicates a compiler-internal
invariant was violated, not a mistake in your source - please report it as a
compiler bug with a minimal repro.

## Known issues (open tickets)

- **GLYPH-66** - no integer-literal `match` patterns; use `if`/`else`.
- **GLYPH-75** - a large literal used inline as an `as` operand truncates to
  `i32` range before the cast is applied.
- **GLYPH-77** - implicit narrowing and same-width signed/unsigned
  conversions are silently accepted with no diagnostic; decision pending on
  whether to require an explicit `as`.
- **GLYPH-78** - a negative integer literal assigned to a float-typed `let`
  produces a silently wrong runtime value instead of being promoted or
  rejected.
- **GLYPH-79** - array-typed struct fields cannot be read or indexed, only
  written at construction time.
- **Untracked** - binary operators (`+`, `==`, ...) do not auto-dereference a
  `&i32` (unlike field access through `&Struct`, which does); depending on
  context this produces either a misleading "value cannot escape through
  return" diagnostic or an outright LLVM IR verifier crash. Found while
  writing this page; needs a ticket.
- **Untracked** - an array literal with more elements than its declared
  fixed size (`let a: [i32; 2] = [1, 2, 3]`) is silently accepted instead of
  being rejected the way an out-of-range element value now is (GLYPH-74).
  Found while writing this page; needs a ticket.
