# Language Basics

Glyph favors a small set of keywords and a readable, expression-oriented style.

## A Small Program

```glyph
from std import println

fn add(a: i32, b: i32) -> i32 {
  a + b
}

fn main() -> i32 {
  let x = add(2, 3)
  println($"x = {x}")
  ret 0
}
```

## Values, Variables, and Types

- `let name = expr` binds an immutable local.
- `let mut name = expr` binds a mutable local.
- You can annotate types: `let v: Vec<i32> = Vec::new()`.

Common built-in types include:

- integers: `i8`, `i32`, `i64`, `u8`, `u32`, `u64`, `usize`
- floating point: `f32`, `f64`
- `bool`, `char`
- strings: `str` (borrowed string), `String` (owned string)

String literals like `"hello"` have type `str`. Use `String::from_str("...")` when you need an owned `String`.

Strings support hex escape sequences for embedding arbitrary byte values:

```glyph
let esc = "\x1B[31m"   // ESC character followed by ANSI color code
let tab = "\x09"        // tab character
```

## Expressions and Return Values

Blocks produce a value: the last expression is the block result.

```glyph
fn abs_i32(x: i32) -> i32 {
  if x < 0 { -x } else { x }
}
```

Use `ret expr` for an explicit return (useful for early exits).

## Operators

Glyph supports the standard arithmetic, comparison, and logical operators:

- Arithmetic: `+`, `-`, `*`, `/`, `%` (modulo — integer remainder)
- Comparison: `==`, `!=`, `<`, `<=`, `>`, `>=`
- Logical: `&&`, `||`, `!`
- Error propagation: `?` (works on `Result` and `Option` values)

## Control Flow

`if` and `match` are expressions, so you can assign their result.

Loops:

```glyph
fn main() -> i32 {
  let i = 0
  while i < 3 {
    i = i + 1
  }

  let sum = 0
  for n in 0..10 {
    sum = sum + n
  }

  ret sum
}
```

`break` and `cont` work inside `while` and `for`.

## Imports

Imports are file-based. Use `from ... import ...` for local names or `import ...`
for qualified access:

```glyph
from std import println
from std/io import File
import std/enums
```

See [Imports](imports.md) for path rules, aliasing, and collisions.

## Structs

Define structs with named fields:

```glyph
struct Point {
  x: i32
  y: i32
}

fn main() -> i32 {
  let p = Point { x: 1, y: 2 }
  let sum = p.x + p.y
  ret sum
}
```

Struct field ownership:

- You cannot move individual fields out of a struct.
- Borrow or clone the field, or move the whole struct.

```glyph
let name: str = user.name
let owned = user.name.clone()
```

## Ownership at Calls

Glyph uses single-owner move semantics for values that own resources.

Passing an owned droppable value to a by-value parameter consumes the caller's
local. This includes `String`, `Own<T>`, `Shared<T>`, `Vec<T>`, `Map<K, V>`,
and structs or enums that contain owned values.

```glyph
struct User {
  name: String
}

fn consume(user: User) -> i32 {
  let name: str = user.name
  ret name.len()
}

fn main() -> i32 {
  let user = User { name: String::from_str("ada") }
  let n = consume(user)
  // `user` has moved here.
  ret n
}
```

Use `&T` or `&mut T` when a function should borrow instead of consume:

```glyph
fn name_len(user: &User) -> i32 {
  let name: str = user.name
  ret name.len()
}
```

Non-owning snapshots from field access, vector indexing, map lookup, and enum
payload extraction are not independent owners. The compiler suppresses drops for
these snapshots to avoid freeing data still owned elsewhere. Prefer borrowing or
an explicit clone/deep-copy operation when an API needs to keep a value.

## Methods

Structs can have methods defined directly inside the struct body. The first
parameter must be named `self` with an explicit type annotation.

### Inherent Methods

Methods declared directly inside a struct (without an `impl` block) are
*inherent methods* — they belong to the struct itself:

```glyph
struct Point {
  x: i32
  y: i32

  fn norm(self: &Point) -> i32 {
    ret self.x * self.x + self.y * self.y
  }
}

fn main() -> i32 {
  let p = Point { x: 3, y: 4 }
  ret p.norm()
}
```

Call methods with dot notation: `p.norm()`. The compiler auto-borrows the
receiver, so you pass a value and it becomes `&Point` automatically.

### Self Parameter

The `self` parameter determines how the receiver is passed:

| Syntax | Meaning |
|--------|---------|
| `self: &Point` | Immutable borrow — reads fields but cannot mutate |
| `self: &mut Point` | Mutable borrow — can read and write fields |
| `self: Point` | By value — consumes the struct |

The `self` type must always be written explicitly (no shorthand like `&self`).

### Mutable Methods

Use `&mut` when a method needs to modify the receiver:

```glyph
struct Counter {
  value: i32

  fn increment(self: &mut Counter) {
    self.value = self.value + 1
  }

  fn get(self: &Counter) -> i32 {
    ret self.value
  }
}
```

### Methods with Extra Parameters

Methods can take additional parameters after `self`:

```glyph
struct Accumulator {
  total: i32

  fn add(self: &Accumulator, value: i32, weight: i32) -> i32 {
    ret self.total + value * weight
  }
}

fn main() -> i32 {
  let acc = Accumulator { total: 10 }
  ret acc.add(2, 3)
}
```

## Interfaces

Interfaces declare a set of method signatures that structs can implement.

### Defining an Interface

```glyph
interface Drawable {
  fn draw(self: &Point) -> i32;
}
```

### Implementing an Interface

Use an `impl` block inside the struct to implement the interface methods:

```glyph
interface Drawable {
  fn draw(self: &Point) -> i32;
}

struct Point {
  x: i32
  y: i32

  impl Drawable {
    fn draw(self: &Point) -> i32 {
      ret self.x + self.y
    }
  }
}

fn main() -> i32 {
  let p = Point { x: 10, y: 20 }
  ret p.draw()
}
```

### Mixing Inherent Methods and Interface Impls

A struct can have both inherent methods and interface implementations:

```glyph
interface Drawable {
  fn draw(self: &Point) -> i32;
}

struct Point {
  x: i32
  y: i32

  fn norm(self: &Point) -> i32 {
    ret self.x * self.x + self.y * self.y
  }

  impl Drawable {
    fn draw(self: &Point) -> i32 {
      ret self.x + self.y
    }
  }
}

fn main() -> i32 {
  let p = Point { x: 3, y: 4 }
  ret p.norm() + p.draw()
}
```

### Multiple Interfaces

A struct can implement multiple interfaces with separate `impl` blocks:

```glyph
interface Drawable {
  fn draw(self: &Point) -> i32;
}

interface Serializable {
  fn serialize(self: &Point) -> i32;
}

struct Point {
  x: i32
  y: i32

  impl Drawable {
    fn draw(self: &Point) -> i32 {
      ret self.x + self.y
    }
  }

  impl Serializable {
    fn serialize(self: &Point) -> i32 {
      ret self.x * 100 + self.y
    }
  }
}
```

## Enums

Enums can be simple tags or carry payload values:

```glyph
enum OptionI32 {
  None
  Some(i32)
}

fn main() -> i32 {
  let a: OptionI32 = OptionI32::Some(123)
  let b: OptionI32 = OptionI32::None
  let _keep_alive = (a, b)
  ret 0
}
```

## match

Use `match` to branch on enum variants.

```glyph
from std/enums import Result
from std/io import File
from std import println

fn main() -> i32 {
  let created = File::create("out.txt")
  let status = match created {
    Ok(file) => {
      let _ = file.close()
      0
    },
    Err(_err) => {
      println("failed")
      1
    },
  }
  ret status
}
```

## Error Propagation

The `?` operator unwraps the success variant or returns early from the enclosing
function.

For `Result<T, E>`, `Ok(value)?` evaluates to `value`; `Err(error)?` returns
`Err(error)` from the current function:

```glyph
from std/enums import Result

fn parse_count() -> Result<i32, String> {
  ret Ok(41)
}

fn next_count() -> Result<i32, String> {
  let count = parse_count()?
  ret Ok(count + 1)
}
```

For `Option<T>`, `Some(value)?` evaluates to `value`; `None()?` returns
`None()` from the current function:

```glyph
from std/enums import Option

fn maybe_count(enabled: bool) -> Option<i32> {
  if enabled { ret Some(41) }
  ret None()
}

fn next_count(enabled: bool) -> Option<i32> {
  let count = maybe_count(enabled)?
  ret Some(count + 1)
}
```

The enclosing function must return the same family: `Result` for `Result?`, or
`Option` for `Option?`.

## Void Functions

Functions that don't return a value omit the `-> Type` annotation:

```glyph
from std import println

fn greet(name: str) {
  println($"hello {name}")
}

fn main() -> i32 {
  greet("world")
  ret 0
}
```

Void function calls can appear anywhere in a block, not just at the end.

## Vectors (Vec)

`Vec<T>` is the standard growable list.

```glyph
fn main() -> i32 {
  let mut v: Vec<i32> = Vec::new()
  v.push(1)
  v.push(2)
  let _popped = v.pop()
  let _len: usize = v.len()
  ret 0
}
```

## Maps (Map)

`Map<K, V>` is a hash map.

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
