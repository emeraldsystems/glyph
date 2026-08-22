# Threads and Shared State

Glyph's first concurrency surface uses owned `FnOnce` tasks, native join
handles, atomic scalars, `Arc<T>`, and `Mutex<T>`. These are intended for
control and worker threads.

```glyph
import Arc from std/sync
import Mutex from std/sync
import spawn from std/thread

let state = Arc::new(Mutex::new(AtomicI32::new(0)))
let worker_state = state.clone()
let task: FnOnce<(), ()> = move () -> {
  let mutex = worker_state.borrow()
  let guard = mutex.lock()
  let value = guard.borrow_mut()
  let previous = value.fetch_add(1)
}
let outcome = spawn(task)
```

`Arc<T>` provides atomic shared ownership but only immutable access.
`Arc<Mutex<T>>` is the standard way to share mutable worker state. `Mutex<T>`
is `Send + Sync` only when `T` is `Send`.

`lock()` blocks and returns a move-only `MutexGuard<T>`. `try_lock()` returns
`Option<MutexGuard<T>>` without blocking. A guard supplies exclusive mutable
access through `borrow_mut()` and unlocks exactly once when its lexical scope
ends, including `ret`, `?`, `break`, and `cont` exits.

The initial loan checker is intentionally conservative. A guard cannot be
returned, captured, sent to another thread, or stored in a struct, tuple,
array, enum, or collection. The locked `Mutex` cannot move or be dropped while
the guard is live. Use a nested block to make the unlock point explicit.

Mutexes are non-reentrant and fairness is unspecified. Glyph does not poison a
mutex because it has no catchable unwind model.

## Hard real-time boundary

Do not use `Mutex`, `Arc`, thread creation/join, or closure allocation and drop
on an audio render/device callback. Mutex operations may block in the kernel;
Arc destruction and callable cleanup may free memory or run arbitrary drop
glue. Prepare immutable state on a control thread and communicate with the
callback through the sequencer's dedicated bounded real-time protocol.
