# Pointers

Glyph has a small set of pointer-like types for borrowing and heap allocation.

## References

`&T` is a borrowed reference to a value.

You can take a reference to a local with `&local`:

```glyph
struct Point {
  x: i32
}

fn get_x(p: &Point) -> i32 {
  ret p.x
}

fn main() -> i32 {
  let point = Point { x: 4 }
  ret get_x(&point)
}
```

Notes:

- References can only be taken to locals (not arbitrary expressions).
- `&str` is the borrowed string type used for string parameters.

## `Own<T>`

`Own<T>` is a single-owner heap allocation.

```glyph
fn make() -> Own<i32> {
  let tmp = Own::new(99)
  ret tmp
}

fn main() -> i32 {
  let heap = make()
  let raw = heap.into_raw()
  let back = Own::from_raw(raw)
  let _keep_alive = back
  ret 0
}
```

Notes:

- `into_raw()` transfers ownership to a `RawPtr<T>`.
- `from_raw()` must be called exactly once for a given raw pointer to avoid leaks or double-free.

## `Shared<T>`

`Shared<T>` is shared ownership (reference-counted). A `Shared<T>` handle moves
by default when passed or assigned by value. Use `.clone()` when you want another
handle to the same allocation.

```glyph
fn main() -> i32 {
  let s1 = Shared::new(7)
  let s2 = s1.clone()
  let _keep_alive = (s1, s2)
  ret 0
}
```

Dropping each cloned handle decrements the reference count. The allocation is
freed when the final handle is dropped.

`Shared<T>` is intentionally single-threaded. It cannot be captured by a task
passed to `spawn`, including when it is nested inside another struct.

## `Arc<T>`

`Arc<T>` provides atomic shared ownership for immutable data that must cross a
thread boundary. Import it from `std/sync`, move one clone into each task, and
keep another clone when the spawning thread still needs access:

```glyph
import Arc from std/sync
import spawn from std/thread

fn main() -> i32 {
  let counter = Arc::new(AtomicI32::new(0))
  let worker_counter = counter.clone()
  let task: FnOnce<(), ()> = move () -> {
    let atomic = worker_counter.borrow()
    let previous = atomic.fetch_add(1)
  }
  let outcome = spawn(task)
  ret 0
}
```

`Arc<T>` supports `new`, explicit `clone`, and immutable `borrow`. It is safe to
transfer only when `T` and every nested field are both thread-transferable and
safe to share; references, raw pointers, and `Shared<T>` therefore cannot be
smuggled into a worker through an `Arc`.

An `Arc::borrow()` view is tied to the exact owner local for the rest of its
lexical scope. The owner cannot be moved or reassigned until that scope ends.
For v1, these views must remain direct local bindings: storing one in an
aggregate, collection, heap owner, enum, or closure is rejected. Use a smaller
nested block to end a view before moving its owner, or clone the `Arc` and move
the clone when ownership must cross a boundary.

The v1 API has no weak handles, mutable access, or raw conversions. Reference
cycles leak by design. Allocation and final destruction are not real-time safe,
and clone/drop are not promised to be wait-free or safe in an audio callback;
prepare immutable assets and configuration outside the real-time path.

## `RawPtr<T>`

`RawPtr<T>` is an unsafe, opaque pointer type primarily used for FFI boundaries.
You can obtain one from `Own<T>::into_raw()`.

In general, prefer `&T`, `Own<T>`, or `Shared<T>` unless you explicitly need raw pointers.
