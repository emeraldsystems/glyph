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

## `RawPtr<T>`

`RawPtr<T>` is an unsafe, opaque pointer type primarily used for FFI boundaries.
You can obtain one from `Own<T>::into_raw()`.

In general, prefer `&T`, `Own<T>`, or `Shared<T>` unless you explicitly need raw pointers.
