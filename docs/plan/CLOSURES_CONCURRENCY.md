# Closures and Concurrency v0: Semantics and ABI

**Status:** Accepted for the GLYPH-32 implementation epic

**Decision owner:** GLYPH-33

**Scope:** Owned `FnOnce` closures, safe native threads, atomics, `Arc<T>`,
`Mutex<T>`, and bounded SPSC communication

This document is normative for the first closures-and-concurrency release. It
locks the language and runtime contracts that the implementation stories share.
Later work may add capabilities, but must not silently weaken these safety
rules.

The older lambda proposal's arrow spelling remains valid. Its borrowed
`Fn`/`FnMut` capture model is superseded for this release: v0 closures own their
captures and are callable once.

## 1. Current baseline and constraints

At the start of this epic:

- calls in MIR name a statically known function; values are not callable;
- the type system has no callable, atomic, `Arc`, mutex, or thread types;
- move/drop tracking exists, including structural drop and aggregate returns;
- `Shared<T>` has a non-atomic reference count and is single-threaded;
- generic-looking stdlib operations are commonly compiler-recognized and
  lowered to concrete MIR rather than implemented as user generic functions;
- the macOS audio callback is implemented in C. Glyph renders blocks on a
  normal thread and submits them through a blocking runtime API. Glyph code
  does not execute on the real-time callback.

These constraints favor a deliberately small first step: one owned callable
kind, structural thread-safety checks, compiler-known generic APIs, and a
bounded communication primitive.

## 2. Closure surface syntax

Arrow syntax is locked. The grammar is:

```text
closure-expr   := [ "move" ] closure-params "->" closure-body
closure-params := closure-param
                | "(" ")"
                | "(" closure-param ("," closure-param)+ [","] ")"
closure-param  := IDENT [ ":" type-expr ]
closure-body   := expression | block
```

Canonical examples:

```glyph
x -> x + 1
(x, y) -> x + y
() -> 42
(x: i32, y: i32) -> { x + y }
move x -> x + offset
move (x, y) -> x + y + offset
move () -> { render_block() }
```

The following parsing rules are part of the decision:

- A single parameter is unparenthesized in canonical source. Parenthesized
  single parameters may be accepted as grouping, but the formatter emits
  `x -> ...`.
- Zero and multiple parameters require parentheses. `x, y -> ...` is invalid.
- Parameters are identifiers with optional type annotations. Patterns,
  defaults, and destructuring are deferred.
- `->` has lower precedence than all ordinary expression operators and is
  right-associative. The body extends as far right as the surrounding
  delimiter permits.
- A closure used where an outer comma would otherwise terminate it is grouped,
  for example `fold(0, (acc, x) -> acc + x)`.
- Parameter and result types use local, bidirectional inference. If a
  parameter cannot be determined from its body or expected callable type, the
  compiler requires an annotation.
- `move` is an explicit capture marker. In v0, both implicit and explicit
  capture forms produce an owned environment; `move` makes the ownership
  transfer visible and is the canonical spelling at escaping and thread
  boundaries.

`async` closures, variadics, generators, pattern parameters, and FFI closure
parameters are not in this release.

## 3. Callable type and `FnOnce` behavior

There is one callable capability in v0: `FnOnce`. The compiler recognizes the
type constructor below; it is not a user-implementable interface:

```text
FnOnce<Args, Return>
```

`Args` is `()` for zero parameters, the parameter type for one parameter, and
a tuple for multiple parameters:

```text
FnOnce<(), i32>
FnOnce<i32, i32>
FnOnce<(i32, i32), i32>
```

A tuple-typed single parameter uses a one-element outer tuple to preserve its
arity: `FnOnce<((i32, i32),), R>`. The trailing comma is semantically
significant and must be preserved by type rendering. This distinguishes it
from `FnOnce<(i32, i32), R>`, which takes two scalar parameters.

A closure literal normally has a unique environment type internally, but it
coerces to the corresponding `FnOnce<Args, Return>` value. A named function
item may coerce to the same callable type. Callable values are move-only and
cannot be cloned.

Calling a callable local consumes it. A second call, a call after another
move, or dropping both the source and a moved copy is a compile error. A named
function may still be called repeatedly because each reference to the function
creates a new environment-free callable value.

`Fn` and `FnMut` are deferred until Glyph has the loan, lifetime, and alias
analysis needed for borrowed and repeatedly callable environments.

### 3.1 Uniform internal ABI

The logical callable representation is three machine pointers:

```text
FnOnceValue {
    env:    *opaque,
    invoke: *code,
    drop:   *code,
}
```

- `env` owns the captured environment, or is null when none is needed.
- `invoke` points to a compiler-generated, signature-specific thunk.
- `drop` destroys an uncalled environment, or is null when no cleanup is
  needed.

The invoke thunk has the logical signature:

```text
invoke(env: *opaque, arg0: A0, ..., argN: AN) -> R
```

It follows Glyph's existing concrete function ABI. When `R` uses an aggregate
return slot, the physical argument order is the existing hidden `sret` pointer,
then `env`, then the explicit arguments. The thunk, direct function, and
indirect call must agree on all integer widths, aggregate passing, and `sret`
attributes.

Invocation transfers ownership of the environment to the thunk. The thunk
moves or drops every capture exactly once and releases environment storage on
every normal exit. The caller marks the callable consumed before transferring
control. If an owned callable is never invoked, ordinary drop calls its `drop`
thunk instead.

Function items and noncapturing closures use a null environment and drop
pointer. They use an environment-first adapter thunk so indirect calls retain
one representation. Direct-call and stack-environment optimizations are
allowed only when they preserve the same observable move/drop behavior.

Environment storage is not a public ABI. The first implementation may heap
allocate nonempty environments. Escape analysis may stack-allocate a proven
nonescaping environment. Callable values are not C-compatible and cannot be
passed through `extern "C"`.

## 4. Capture and escape rules

Capture analysis finds free local bindings referenced by a closure body after
name resolution. Function items, constants, and globals are not captures.
Each captured local is transferred when the closure expression is evaluated:

- move-only values move into the environment;
- trivially copyable values are copied into the environment;
- using a moved source binding afterward is rejected;
- nested closures propagate the ownership requirement through their enclosing
  environments;
- the generated environment field order is deterministic (source binding
  order), although it is not a stable external ABI.

`CaptureMode::Inferred` means that the compiler infers the *set* of free
locals. It does not infer borrowed versus owned capture. `CaptureMode::Move`
uses the same v0 ownership rules while recording explicit programmer intent.

The v0 environment must be self-contained. Capturing `&T`, `&mut T`, `str`, or
an aggregate that transitively contains a borrowed view is rejected. A string
literal may be referenced directly as static data; a runtime string view must
be converted to an owned `String` before capture. This conservative rule is
intentional: borrowed captures and their nonescape proof belong to the later
`Fn`/`FnMut` and scoped-thread work.

Because environments own all captures, they may escape their defining block by
being returned, stored, or passed to another function once the destination
supports the callable type. Thread escape adds the structural `Send` checks in
Section 6. Dynamic callable trait objects, closure cloning, and closure
serialization are deferred.

## 5. Compiler-intrinsic generic-looking APIs

The following signatures specify type behavior. They do not imply that user
generic free functions, interface bounds, or higher-ranked lifetimes are
available. The resolver recognizes the canonical stdlib symbol, infers each
concrete `T` from arguments or expected type, and lowers a concrete
specialization to MIR/runtime calls. A user function with the same short name
must not gain intrinsic behavior.

```text
spawn<T>(task: FnOnce<(), T>)
    -> Result<JoinHandle<T>, ThreadError>

channel<T>(capacity: usize)
    -> Result<(Sender<T>, Receiver<T>), ChannelError>

Arc<T>::new(value: T) -> Arc<T>
Arc<T>::clone(self: &Arc<T>) -> Arc<T>
Arc<T>::get(self: &Arc<T>) -> &T

Mutex<T>::new(value: T) -> Mutex<T>
Mutex<T>::lock(self: &Mutex<T>) -> MutexGuard<T>
MutexGuard<T>::get(self: &MutexGuard<T>) -> &T
MutexGuard<T>::get_mut(self: &mut MutexGuard<T>) -> &mut T
```

If inference leaves `T` unknown, an expected type annotation is required. No
runtime type descriptors or type erasure are introduced by these APIs.

## 6. Structural `Send` and `Sync`

`Send` means ownership of a value may move to another thread. `Sync` means the
type supports safe shared access from multiple threads. In v0 these are
compiler predicates, not user-declared interfaces. There is no `unsafe impl`
escape hatch.

The compiler evaluates them structurally and reports the field/capture path
that made a type fail. The minimum rules are:

| Type | `Send` | `Sync` |
|---|---|---|
| Numeric scalars, `bool`, `char`, `()` | yes | yes |
| `String` | yes | yes |
| Struct/enum/tuple/array | iff every component is | iff every component is |
| `Own<T>`, `Vec<T>`, `Map<K,V>` | iff owned components are `Send` | iff shared observation of every component is `Sync` |
| `Shared<T>` | no | no |
| `RawPtr<T>` | no | no |
| Borrowed `str`, `&T`, `&mut T` | never thread-escaping in v0 | never thread-escaping in v0 |
| Atomic scalar types | yes | yes |
| `Arc<T>` | iff `T: Send + Sync` | iff `T: Send + Sync` |
| `Mutex<T>` | iff `T: Send` | iff `T: Send` |
| `MutexGuard<T>` | no | no |
| `FnOnce<A,R>` | iff every capture is `Send` | no |
| `JoinHandle<T>` | iff `T: Send` | no |
| `Sender<T>`, `Receiver<T>` | iff `T: Send` | no |

`spawn` requires both its closure and result `T` to be `Send`. This checks the
entire capture environment, not only values used on a particular branch.
Wrapping a non-`Send` value in `Arc` does not launder it into a thread-safe
type.

## 7. Atomic memory model

The initial public atomic surface contains at least `AtomicBool` and
`AtomicUsize`. They are value types with required native alignment and may only
be accessed through atomic operations. The minimum operations are:

```text
AtomicBool:  new, load, store, swap, compare_exchange
AtomicUsize: new, load, store, swap, compare_exchange, fetch_add, fetch_sub
```

Every public atomic operation is sequentially consistent (`SeqCst`). v0 has no
public ordering parameter, fence API, or safe `AtomicPtr<T>`. This makes source
semantics small and prevents callers from accidentally weakening a safety
protocol. Integer read/modify/write overflow follows the corresponding
unsigned wrapping operation.

Compiler/runtime internals use the weakest ordering required by their locked
protocols:

- publication uses a release store or release read/modify/write;
- observation pairs it with an acquire load or acquire fence;
- an `Arc` final decrement is release, followed by an acquire fence before
  destroying `T`;
- an `Arc` increment may be relaxed because it does not publish the pointee;
- the SPSC producer publishes initialized slots with release and the consumer
  observes them with acquire; the reverse head update uses the same pairing.

All orderings must lower to LLVM atomics and, through LLVM, the target's native
memory model. A plain load/store of atomic storage is a compiler error. An
unsupported target must fail explicitly rather than silently lower atomics to
non-atomic accesses.

## 8. `Arc<T>`: atomic shared ownership

`Arc<T>` is distinct from `Shared<T>`. Existing `Shared<T>` remains a faster,
non-atomic, single-threaded reference count and is permanently non-`Send` and
non-`Sync` for this release.

An `Arc` allocation logically contains an atomic strong count and `T`. `new`
starts at one, `clone` increments, and drop decrements. Exactly one thread that
observes the transition to zero runs the acquire fence, drops `T`, and frees
the allocation. Counter overflow aborts before wrapping because wraparound
could cause premature destruction. Null is not a valid live `Arc` value.

`Arc<T>` guarantees allocation lifetime, not mutation safety. Its safe access
surface yields only `&T`. Shared mutation requires an atomic, `Arc<Mutex<T>>`,
or message passing. `Arc<Shared<T>>` remains non-`Send` because the inner
non-atomic owner is not thread-safe.

Weak counts/references, cyclic collection, custom allocators, `Arc::get_mut`,
and safe `AtomicPtr<T>` are deferred.

## 9. `Mutex<T>` and guard escape

`Mutex<T>` provides blocking mutual exclusion and owns its `T`. It is suitable
for ordinary worker/control threads, not the audio callback. `lock` blocks
until it acquires the mutex and returns a guard. Glyph does not introduce
poisoning in v0; an impossible runtime mutex invariant failure aborts rather
than returning an unlocked guard.

`MutexGuard<T>` is a compiler-known RAII guard. Its drop glue unlocks exactly
once on normal block exit, `ret`, `break`, and `cont`. The guard exposes data
only through borrows returned by `get`/`get_mut`.

Until full lifetime analysis exists, a mutex guard is explicitly `noescape`:

- it may exist only in a local binding in the lock's lexical scope;
- it cannot be returned, placed in a struct/enum/tuple/container, captured by
  a closure, moved into a thread, or assigned to a longer-lived binding;
- borrows obtained from it cannot outlive it;
- the owning mutex cannot move or drop while a guard is live.

Guards are neither `Send` nor `Sync`, even when `T` is. Recursive mutexes,
read/write locks, timed locks, condition variables, and fairness guarantees are
deferred.

## 10. Native thread lifecycle

The initial runtime uses joinable native threads (pthreads on supported Unix
targets) behind `std/thread`.

`spawn` consumes an owned, zero-argument `FnOnce` closure. On success, the new
thread exclusively owns the closure and invokes it once. On creation failure,
`spawn` drops the consumed closure and returns `Err(ThreadError)`. The entry
trampoline stores either the concrete return value or runtime failure state in
shared join storage and releases every remaining capture before exiting.

`JoinHandle<T>::join(self)` consumes the handle and returns
`Result<T, ThreadError>`. Exactly one join is possible. The successful join
acquires completion, moves `T` to the joining thread, and releases the join
storage. Glyph currently has no language panic/unwind payload; a future panic
model must extend, not reinterpret, `ThreadError`.

Dropping an unjoined handle detaches the native thread. It does not cancel or
join it. The detached thread keeps its state alive until completion, then drops
an unclaimed result and frees the state. Process termination does not wait for
detached threads. Forced cancellation, scoped threads, thread priorities,
names, affinity, and deadlines are deferred.

## 11. Bounded SPSC channel

`channel<T>(capacity)` creates exactly one move-only `Sender<T>` and one
move-only `Receiver<T>`. Neither endpoint is cloneable, preserving the
single-producer/single-consumer proof structurally. Capacity is fixed, must be
greater than zero, and allocates all ring storage during construction.

The minimum nonblocking operations are:

```text
Sender<T>::try_send(self: &mut Sender<T>, value: T)
    -> Result<(), SendError<T>>

Receiver<T>::try_recv(self: &mut Receiver<T>)
    -> Result<T, RecvError>
```

`try_send` moves `value` into one initialized slot or returns it in
`SendError<T>` when the ring is full/disconnected. `try_recv` moves one value
out or reports empty/disconnected. It must be possible to distinguish a
temporarily empty channel from a closed and drained one.

The ring uses independent producer and consumer indices. The producer writes a
slot before a release publication; the consumer acquire-loads that publication
before reading the slot. The consumer release-publishes freed capacity, paired
with the producer's acquire observation. There are no locks, syscalls, or
allocations in a successful or full/empty `try_*` operation after channel
construction.

Dropping an endpoint release-publishes disconnection. The remaining endpoint
acquire-observes it. When both endpoints are gone, every still-initialized `T`
is dropped exactly once and the ring storage is freed. Blocking send/receive,
MPMC/MPSC channels, endpoint cloning, selection, and async wakeups are deferred.

## 12. Real-time audio boundary and non-goals

This epic enables sequencer and synthesis architecture; it does not claim a
hard real-time Glyph runtime.

- Glyph closures do not run on the platform audio callback in v0. The existing
  C callback and prefilled-buffer architecture remains the boundary.
- Thread creation/join, `Mutex`, blocking audio writes, allocation, `Arc` last
  drop, arbitrary `T` drop glue, printing, file I/O, and public `SeqCst`
  atomics are not real-time-safe operations.
- SPSC `try_send`/`try_recv` have a lock-free, allocation-free control path,
  but a blanket real-time guarantee is impossible for arbitrary `T` because
  moving/dropping `T` may execute non-real-time work.
- The sequencer acceptance slice should construct and allocate outside the
  render loop, communicate bounded values/handles, and continue submitting
  completed blocks through the current runtime audio API.
- Real-time scheduling policy, priority inversion mitigation, a no-allocation
  effect system, callback-safe destructors, and executing Glyph DSP directly
  in a device callback are separate future designs.

## 13. Required diagnostics

Diagnostics must identify both the operation and the ownership/thread-safety
path. At minimum:

- ambiguous closure parameter or return type;
- `FnOnce` with anything other than exactly two type arguments;
- malformed arrow parameter list, especially missing parentheses for zero or
  multiple parameters;
- use of a capture after it moved into an environment;
- second invocation or use after move of `FnOnce`;
- borrowed or `str` capture in an owned v0 closure;
- closure/thread escape containing the first non-`Send` capture and nested
  field path;
- attempted shared `Arc<T>` mutation without an interior synchronization type;
- mutex guard or guard-derived borrow escape;
- cloning an SPSC endpoint;
- non-atomic access to atomic storage;
- unsupported atomics or threads on a target.

## 14. Compile-pass matrix

Each row requires parser, resolver/type, MIR, backend, and compiled-program
coverage where applicable.

| Case | Representative source/behavior | Required result |
|---|---|---|
| Arrow arities | `x -> x + 1`, `(x, y) -> x + y`, `() -> 1` | Parses and infers with context |
| Typed parameters | `(x: i32, y: i32) -> x + y` | Produces `FnOnce<(i32,i32),i32>` |
| Tuple parameter | `(pair: (i32, i32)) -> pair` | Produces `FnOnce<((i32,i32),), (i32,i32)>` |
| Explicit move | `let f = move () -> owned` | `owned` transfers at closure creation |
| Function item | Bind a named function to a callable local and call it | Environment-free indirect call succeeds |
| Owned capture drop | Create but do not call a closure capturing `String` | Environment drop frees the string once |
| Owned capture invoke | Call a closure capturing `String` | Invoke path frees/moves captures once |
| Aggregate result | Closure returns a struct/enum/string aggregate | Indirect `sret` ABI matches direct ABI |
| Thread result | Spawn `move () -> 42`, then join | Join returns `Ok(42)` once |
| Detached result | Spawn an owned task and drop its handle | Task/result/captures eventually drop without leak |
| Structural send | Move `String`, `Own<i32>`, or an all-`Send` struct into a task | Compiles |
| Atomic publication | One thread stores, another loads an atomic flag | Public operations are `SeqCst` and race-free |
| Arc lifetime | Clone `Arc<String>` across tasks and join | Last owner drops data once |
| Mutex sharing | Share `Arc<Mutex<i32>>`, lock/update on workers | Guard unlocks on every control-flow exit |
| SPSC transfer | Move sender to producer and receiver to consumer | Ordered values arrive exactly once |
| SPSC pressure | Fill ring, drain ring, drop either endpoint | Full/empty/closed states and cleanup are correct |
| Audio acceptance | Sequencer worker sends bounded events/blocks to normal render loop | No Glyph code enters the device callback |

## 15. Compile-fail matrix

| Case | Representative source/behavior | Required diagnostic |
|---|---|---|
| Multi-parameter punctuation | `x, y -> x + y` | Multiple parameters require parentheses |
| Zero-parameter punctuation | `-> 1` | Zero parameters require `()` |
| Ambiguous parameter | `let f = x -> x` with no expected type | Add parameter/callable type annotation |
| Callable type arity | `FnOnce<i32>` or `FnOnce<i32,i32,i32>` | `FnOnce` expects exactly two type arguments |
| Use after capture | Create `move () -> owned`, then use `owned` | Value moved into closure |
| Call twice | Invoke the same callable local twice | `FnOnce` already consumed |
| Borrowed capture | Capture `&T`, `&mut T`, or runtime `str` | Borrowed captures are deferred; own/clone the value |
| Non-`Send` capture | Spawn a closure containing `Shared<T>` | Capture path is not `Send` |
| Raw pointer capture | Spawn a closure containing `RawPtr<T>` | Raw pointers are not `Send` |
| Arc laundering | Spawn with `Arc<Shared<T>>` | Nested `Shared<T>` prevents `Send + Sync` |
| Unsynchronized Arc mutation | Attempt mutable access through `Arc<T>` | `Arc` exposes immutable access only |
| Guard return | Return `MutexGuard<T>` from a function | Guard is `noescape` |
| Guard storage/capture | Put a guard in an aggregate or closure | Guard is `noescape` and non-`Send` |
| Mutex move while locked | Move/drop mutex with a live guard | Mutex is borrowed by its guard |
| Endpoint clone | Clone a `Sender<T>` or `Receiver<T>` | SPSC endpoints are move-only |
| Endpoint sharing | Attempt simultaneous shared endpoint use | Endpoint is not `Sync`; exclusive owner required |
| Atomic plain access | Read/write atomic backing value as an ordinary scalar | Atomic storage requires atomic methods |
| Public weak ordering | Request `Relaxed`/`Acquire` on a public atomic method | Ordering parameters are not part of v0 API |
| Closure FFI | Pass a callable to `extern "C"` | Callable ABI is Glyph-internal |

## 16. Runtime and release verification

Compilation tests alone are insufficient. The release gate also requires:

- stress loops for concurrent `Arc` clone/drop and final destruction;
- mutex contention plus early-return/break/continue guard-drop tests;
- repeated spawn/join/detach and spawn-failure cleanup tests;
- SPSC wraparound, saturation, disconnect, and destructor-count stress tests;
- ThreadSanitizer runs for the C/runtime protocols where supported;
- LLVM IR assertions for atomic orderings and the indirect aggregate-return
  calling convention;
- the full workspace `cargo test` suite.

No story may describe threads as safe merely because it uses native atomics.
Safety is the combination of owned closures, structural `Send`/`Sync`, the
locked atomic protocols, deterministic drop, and rejection of unsupported
escape patterns.

## 17. Explicitly deferred work

- borrowed `Fn`/`FnMut` closures and general loan/region analysis;
- scoped threads and references crossing a proven scope;
- weak `Arc` references and cycles;
- safe `AtomicPtr<T>` and public memory-order selection;
- async tasks/futures and channel selection;
- MPSC/MPMC channels;
- real-time scheduling or direct Glyph audio callbacks;
- user implementations of `Send`, `Sync`, or callable traits.
