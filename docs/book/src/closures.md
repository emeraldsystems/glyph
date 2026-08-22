# Owned Closures and Callable Values

Glyph's first callable-value model is deliberately small: every closure is an
owned, move-only `FnOnce` value. It can be stored, passed, or returned, and it
can be invoked exactly once.

The normative language and ABI contract is
[Closures and Concurrency v0](https://github.com/emeraldsystems/glyph/blob/master/docs/plan/CLOSURES_CONCURRENCY.md).
This chapter is the user guide; if another historical planning page disagrees,
the v0 contract is authoritative.

## Arrow Syntax

Closure parameters appear to the left of `->`. Zero and multiple parameters
require parentheses; a single parameter does not.

```glyph
let zero: FnOnce<(), i32> = () -> 42
let one: FnOnce<i32, i32> = value -> value + 1
let many: FnOnce<(i32, i32), i32> = (left, right) -> left + right
```

The body can be an expression or a value-producing block:

```glyph
let add: FnOnce<(i32, i32), i32> = (left, right) -> {
  left + right
}
```

Parameter annotations are optional when an expected `FnOnce` type supplies
them. Otherwise, annotate the parameter:

```glyph
let contextual: FnOnce<i32, i32> = value -> value + 1
let explicit = (value: i32) -> value + 1
```

`->` is right-associative, so a nested callback can be written as follows:

```glyph
let nested: FnOnce<i32, FnOnce<i32, i32>> =
  (left: i32) -> (right: i32) -> left + right
```

For this release, bind a closure before calling it. Immediate invocation such
as `((value: i32) -> value + 1)(41)` is not yet supported.

## The `FnOnce` Type

`FnOnce<Args, Return>` always has exactly two type arguments. `Args` encodes
arity:

| Parameters | Callable type |
|---|---|
| none | `FnOnce<(), R>` |
| one `i32` | `FnOnce<i32, R>` |
| two `i32`s | `FnOnce<(i32, i32), R>` |
| one tuple | `FnOnce<((i32, i32),), R>` |

The extra one-element tuple in the last row distinguishes one tuple parameter
from two scalar parameters.

Calling a callable consumes it:

```glyph
let answer: FnOnce<(), i32> = () -> 42
let first = answer()
// answer()  // error: use of moved value `answer`
```

Named functions coerce to compatible `FnOnce` values. Each reference to the
function creates a fresh environment-free callable, so the function itself is
still reusable.

```glyph
fn increment(value: i32) -> i32 { ret value + 1 }

fn apply(callback: FnOnce<i32, i32>, value: i32) -> i32 {
  ret callback(value)
}

fn main() -> i32 {
  ret apply(increment, 41)
}
```

One-shot callbacks fit operations that invoke the callback once. A conventional
iterator that repeatedly invokes the same callback requires the deferred
borrowed `Fn` or `FnMut` capabilities.

## Captures and `move`

A closure owns a self-contained environment. Copy values are copied; owned
values such as `String` move into it. The `move` keyword makes that transfer
explicit and is the recommended spelling when returning a closure or sending
one to another thread.

```glyph
fn make_adder(offset: i32) -> FnOnce<i32, i32> {
  ret move value -> offset + value
}
```

After an owned capture moves into a closure, its old binding cannot be used:

```glyph
let message: String = String::from_str("hello")
let length: FnOnce<(), usize> = move () -> message.len()
// message.len()  // error: `message` moved into the closure
```

Nested closures carry transitive captures, and an uncalled closure drops its
owned captures when the closure itself leaves scope.

Borrowed captures (`&T`, `&mut T`, and runtime `str` views) cannot escape in
this release. Convert data to an owned value before returning, storing, or
passing the closure. Recursive closure cycles are also rejected. These rules
keep the environment valid without a general lifetime/loan system.

## Storing, Passing, and Returning

Callable values use ordinary move semantics:

```glyph
fn apply(callback: FnOnce<i32, i32>, value: i32) -> i32 {
  ret callback(value)
}

fn make_offset(offset: i32) -> FnOnce<i32, i32> {
  ret move value -> offset + value
}

fn main() -> i32 {
  let stored = make_offset(40)
  ret apply(stored, 2)
}
```

Moving `stored` into `apply` consumes the local. A callable's argument count
and types must exactly match its `FnOnce` signature. Calling a scalar or other
non-callable value is a compile error.

## Allocation, FFI, and Audio Code

A capturing closure may allocate its environment on the heap. Invocation and
drop may release that storage and run capture destructors. Callable values are
therefore not safe to create, invoke, or destroy in a hard real-time audio
callback.

Construct callbacks on a control or worker thread and keep Glyph code outside
the device callback. `FnOnce` values use a Glyph-internal ABI and cannot be
passed directly through `extern "C"`; use a purpose-built C trampoline and
state protocol at an FFI boundary.

See the runnable
[owned closure example](https://github.com/emeraldsystems/glyph/tree/master/examples/closures).
