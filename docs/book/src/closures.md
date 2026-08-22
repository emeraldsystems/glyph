# Closures and Callable Values

Glyph has three callable capabilities. An owned, move-only `FnOnce` can be
stored, passed, or returned and is invoked at most once. A borrowed `Fn` or
`FnMut` can be invoked repeatedly, but remains a direct lexical value and
cannot escape the scope of the values it captures.

The normative language and ABI contract is
[Closures and Concurrency](https://github.com/emeraldsystems/glyph/blob/master/docs/plan/CLOSURES_CONCURRENCY.md).
This chapter is the user guide; if another historical planning page disagrees,
that contract is authoritative.

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

Parameter annotations are optional when an expected callable type supplies
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

## Callable Capabilities

`FnOnce<Args, Return>`, `Fn<Args, Return>`, and `FnMut<Args, Return>` always
have exactly two type arguments. `Args` encodes arity:

| Parameters | Callable type |
|---|---|
| none | `FnOnce<(), R>` |
| one `i32` | `FnOnce<i32, R>` |
| two `i32`s | `FnOnce<(i32, i32), R>` |
| one tuple | `FnOnce<((i32, i32),), R>` |

The extra one-element tuple in the last row distinguishes one tuple parameter
from two scalar parameters.

Calling an `FnOnce` consumes it:

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

Use `Fn` for a repeatedly called environment that only reads its captures:

```glyph
fn apply_twice(callback: Fn<i32, i32>, value: i32) -> i32 {
  let first: i32 = callback(value)
  ret first + callback(value)
}

let offset: i32 = 1
let add: Fn<i32, i32> = (value: i32) -> offset + value
```

Use `FnMut` when repeated calls mutate a captured binding. The callable and
the captured binding must both be mutable:

```glyph
struct Counter { value: i32 }

let mut counter: Counter = Counter { value: 0 }
let mut next: FnMut<(), i32> = () -> {
  counter.value = counter.value + 1
  counter.value
}
let first: i32 = next()
let second: i32 = next()
```

`Fn` takes shared capture loans. `FnMut` takes exclusive loans for captures it
mutates and cannot be copied, assigned to another local, or aliased through
multiple arguments. Loans last until the callable's lexical block ends; use a
nested block when the owner must be used again sooner. This first loan checker
is deliberately lexical rather than non-lexical.

## Captures and `move`

An owned closure has a self-contained environment. Copy values are copied;
owned values such as `String` move into it. The `move` keyword makes that
transfer explicit and is the recommended spelling when returning a closure or
sending one to another thread.

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

Nested owned closures carry transitive captures, and an uncalled `FnOnce`
drops its owned captures when the closure itself leaves scope.

The expected callable type selects capture behavior. `Fn` and `FnMut` closure
environments are stack-backed borrowed views; a `move` closure is always an
owned `FnOnce`. Borrowed callables may appear only as direct parameters or
local bindings. They cannot be returned or stored in a struct, enum, tuple,
array, collection, heap owner, `Arc`, `Mutex`, or owned closure. Convert the
captured state to an owned value and use `FnOnce` when the callable must
escape. Recursive closure cycles are also rejected.

## Storing, Passing, and Returning Owned Callables

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
and types must exactly match its signature. Calling a scalar or other
non-callable value is a compile error. Borrowed `Fn`/`FnMut` values may be
passed to direct borrowed-callback parameters, but the callee cannot retain
them.

## Allocation, FFI, and Audio Code

An owned capturing closure may allocate its environment on the heap.
Invocation and drop may release that storage and run capture destructors.
Borrowed callables avoid an owned environment allocation, but invoking
arbitrary Glyph code still has no hard-real-time guarantee. Callable values
are therefore not safe to create, invoke, or destroy in a hard real-time audio
callback.

Construct callbacks on a control or worker thread and keep Glyph code outside
the device callback. All three callable capabilities use a Glyph-internal ABI
and cannot be passed directly through `extern "C"`; use a purpose-built C
trampoline and state protocol at an FFI boundary.

See the runnable
[owned closure example](https://github.com/emeraldsystems/glyph/tree/master/examples/closures).
