# Threads and Shared State

Glyph's concurrency surface provides owned `FnOnce` tasks, lexically scoped
`Fn`/`FnMut` tasks, native join handles, atomic scalars, `Arc<T>`, `Mutex<T>`,
and bounded SPSC channels. These are intended for control and worker threads.

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

`Arc<T>` atomically protects the pointee's lifetime; it is not an atomic raw
pointer and does not make arbitrary mutation safe. Glyph does not expose a
safe `AtomicPtr<T>` in this release. Pointer replacement needs an ownership and
reclamation protocol beyond an atomic address update, so use `Arc<Mutex<T>>`,
an atomic scalar, or message passing instead.

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

## Scoped threads

`std/thread::scope` permits a child task to borrow local state. It invokes a
borrowed callback with a lexical `Scope` token. `Scope::spawn` accepts a
zero-argument `Fn` or `FnMut` and returns a `ScopedJoinHandle<T>`:

```glyph
import scope from std/thread
import Scope from std/thread
import ScopedJoinHandle from std/thread
import ThreadError from std/thread
import Result from std/enums

fn main() -> Result<Result<i32, ThreadError>, ThreadError> {
  let base: i32 = 40
  ret scope((thread_scope: Scope) -> {
    let local: i32 = 2
    let task: Fn<(), i32> = () -> base + local
    let handle: ScopedJoinHandle<i32> = thread_scope.spawn(task)?
    ret handle.join()
  })
}
```

The outer `Result` reports scope creation or drain failure. An explicit
`join()` consumes a handle and reports the child's result. A handle may also be
left unjoined: scope exit still joins that child and drops an unclaimed result
exactly once.

Every callback exit—including `ret`, `?`, `break`, and `cont` cleanup—drains
its children before captured locals are destroyed. `Scope` and
`ScopedJoinHandle<T>` are compiler-issued noescape tokens: they may exist only
in direct lexical bindings, cannot be returned or stored in aggregates, and a
scoped handle cannot detach. Nested spawning from a worker by capturing a
`Scope` token is not supported.

A shared capture requires its referent to be `Sync`; a mutable capture and a
task result require `Send`. The current checker conservatively reserves a
task's loans until scope drain, even after an explicit join. Consequently one
`FnMut` task value cannot have two live spawns. Put shorter-lived work in a
nested `scope` when the captured owner must become available earlier.

## Bounded SPSC channels

`std/sync/spsc` provides a bounded, nonblocking channel for exactly one
producer and one consumer. Construct it with a positive capacity:

```glyph
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult

let mut receiver: Receiver<i32>
let sender: Sender<i32> = channel(256, &mut receiver)
```

The two-argument form above is convenient until Glyph gains tuple
destructuring. The canonical one-argument form is also available when the
pair can be passed or stored as a tuple:

```glyph
let pair: (Sender<i32>, Receiver<i32>) = channel(256)
```

`sender.try_send(value)` returns `Sent`, `Full(value)`, or
`Disconnected(value)`. Both failure variants return the unsent value to the
caller, so sending never silently destroys ownership. `receiver.try_recv()`
returns `Value(value)`, `Empty`, or `Disconnected`.

Endpoints are move-only: cloning either endpoint is rejected, preserving the
single-producer/single-consumer invariant. `Sender<T>` and `Receiver<T>` may
cross a thread boundary only when `T` is `Send`. Dropping an endpoint closes
its side; the last endpoint destroys every queued value exactly once.

Construction makes one fixed-capacity allocation. After that, `try_send` and
`try_recv` use acquire/release atomics and perform no allocation, locking, or
system call per message. They are therefore the intended primitive for a
pre-allocated sequencer-to-render event path. Endpoint destruction can drain
values and free memory, so arrange for final endpoint drops on an ordinary
control or render thread.

The tracked sequencer acceptance example and its metric definitions now
live with the GlyphAudio engine; [the sequencer chapter](sequencer.md)
walks through it. Its offline golden uses sample-frame timestamps for
deterministic rendering; its opt-in live producer uses absolute monotonic
`sleep_until_ns` deadlines and reports scheduler jitter without ever
running Glyph code in the device callback.

## Hard real-time boundary

Do not use `Mutex`, `Arc`, thread creation/join, or closure allocation and drop
on a hard-real-time device callback. Mutex operations may block in the kernel;
Arc destruction and callable cleanup may free memory or run arbitrary drop
glue. In the current audio architecture, SPSC connects ordinary Glyph control,
sequencer, and render loops. Glyph submits prefilled native buffers through the
blocking audio API; the native device callback only consumes those buffers and
never invokes Glyph code. Running Glyph DSP or SPSC operations directly in
that callback is a future protocol, not a current guarantee.
