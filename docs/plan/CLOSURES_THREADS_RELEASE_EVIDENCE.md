# Closures, Threads, and Atomicity Release Evidence (GLYPH-48)

**Status:** Complete.

**Release candidate:** `4ef0722`

**Platforms:** macOS 26.5.2 arm64; Debian 13 arm64 container

This page is the final index for GLYPH-32 evidence. It does not replace the
normative semantics in [CLOSURES_CONCURRENCY.md](CLOSURES_CONCURRENCY.md), the
runtime audit in [RUNTIME_THREAD_SAFETY_AUDIT.md](RUNTIME_THREAD_SAFETY_AUDIT.md),
or the detailed sanitizer transcript in
[CONCURRENCY_SANITIZER_EVIDENCE.md](CONCURRENCY_SANITIZER_EVIDENCE.md).

## Release gates

- [x] `cargo fmt --all -- --check`
- [x] Clippy completed with the documented existing warning baseline.
- [x] `cargo test --workspace --all-features --no-fail-fast -- --test-threads=4`
- [x] `mdbook build docs/book`
- [x] `just build && just install`
- [x] Native and JIT acceptance tests pass on macOS.
- [x] Native and JIT acceptance tests pass on Linux.
- [x] AddressSanitizer runtime, Arc, mutex, SPSC, and scoped-thread suites pass.
- [x] ThreadSanitizer runtime, Arc, mutex, SPSC, and scoped-thread suites pass,
      or a platform limitation and equivalent evidence are recorded.
- [x] No ignored tests or unreviewed generated snapshots remain.
- [x] Every GLYPH-32 child ticket links its implementation commit and focused
      evidence.

## Semantic acceptance

- [x] Owned `FnOnce` invocation and uncalled-drop paths release captures once.
- [x] Borrowed `Fn` repeats with shared capture loans and cannot mutate them.
- [x] Borrowed `FnMut` repeats with exclusive capture loans and cannot alias.
- [x] Borrowed callables cannot escape through return, storage, ownership, FFI,
      or an unscoped thread.
- [x] Typed owned-thread join, detach, failure, and retry paths preserve exact
      result and capture ownership.
- [x] Scoped children drain before callback locals on every control-flow exit;
      explicit and implicit joins release typed results exactly once.
- [x] Structural `Send`/`Sync` diagnostics report the failing capture or field
      path; `Shared<T>` and raw pointers cannot be laundered through `Arc<T>`.
- [x] Public atomics remain sequentially consistent; internal Arc and SPSC
      orderings match their acquire/release protocols in emitted LLVM IR.
- [x] `Arc<T>` last-owner races, mutex guard cleanup, and SPSC saturation,
      wraparound, disconnect, and destructor stress tests pass.
- [x] The sequencer golden render is deterministic; its documented metrics and
      live probe keep Glyph code off the device callback.

## Focused command record

All final runs were performed on 2026-08-22 against the tree committed as
`4ef0722`.

- macOS full suite:
  `cargo test --workspace --all-features --no-fail-fast -- --test-threads=4`
  passed with zero failures or ignored tests.
- macOS focused backend: all backend concurrency tests passed, including 12
  scoped codegen and 5 scoped runtime tests.
- macOS focused source: Arc 3, borrowed closures 6, typed threads 8, Mutex 4,
  SPSC 2, scoped threads 2, and sequencer 3 tests passed.
- Linux focused backend: 66 tests passed on Debian 13 arm64, Rust 1.98.0,
  LLVM/Clang 20.1.8. This includes native object/link execution and JIT paths.
- Linux focused source: 27 tests passed across borrowed closures, typed
  threads, Arc, Mutex, SPSC, scoped threads, and sequencer native/JIT paths.
- ASan and TSan: Arc 6, Mutex codegen 5, Mutex runtime 4, SPSC 5, owned-thread
  runtime 10, scoped codegen 11, and scoped runtime 5 passed under each tool.
  The two nested AOT linker cases were filtered as documented in the sanitizer
  record and pass in ordinary macOS and Linux runs.
- Tooling: glyphfmt 5, glyphlsp 7, mdBook build, release build, and install
  smoke passed.

## GLYPH-2 ownership compatibility

The concurrency surface preserves GLYPH-2's implemented single-owner rule:
passing an owned droppable value by value consumes the caller's local, while
reference parameters borrow it. Closure capture, thread transfer, typed thread
results, channel send, and synchronization wrappers all reuse the same MIR
move/drop accounting; they do not introduce a shallow-copy ownership path.

The focused compatibility run used:

```bash
cargo test -p glyph-cli --all-features \
  --test redteam_edge_cases \
  --test view_escape_ownership \
  --test vec_string_redteam -- --test-threads=4
```

It passed 42 tests with zero failures or ignored tests. The matrix includes
droppable structs crossing ordinary calls, if/match branches, fields and map
views crossing ownership boundaries, nested vectors, returned fields, and
repeated drop stress. The closure/thread/Arc/Mutex/SPSC suites listed above add
the corresponding transfer and cleanup coverage for every new carrier.

The final ownership audit also added and passed:

- two extern-boundary tests rejecting owned droppable by-value parameters while
  accepting call-scoped references, scalars, `str`, and `RawPtr<T>`;
- an invoked `FnOnce` test proving an owned capture and closure environment are
  each released exactly once; and
- scoped `String` result JIT/AOT coverage plus exact-once `Own<i32>` cleanup for
  explicit join and implicit unclaimed-result drain.

The unmodified strict Clippy invocation reaches one denied pre-existing lint in
`glyph_process_run`: a public C ABI function dereferences raw pointers without
being declared `unsafe`. The same code is present on parent `master` at
`918b08a`. The release run therefore used:

```bash
cargo clippy --workspace --all-targets --all-features -- \
  -A clippy::not_unsafe_ptr_arg_deref
```

It completed successfully; the remaining warnings are the repository's
existing warning baseline rather than concurrency release errors.

Minimum focused suites include:

```bash
cargo test -p glyph-frontend --all-features \
  --test borrowed_callable_lowering \
  --test lexical_borrow_checking \
  --test scoped_thread_resolution \
  --test scoped_thread_lowering

cargo test -p glyph-backend --all-features \
  --test scoped_thread_codegen \
  --test scoped_thread_runtime

cargo test -p glyph-cli --all-features \
  --test closure_values \
  --test typed_thread_values \
  --test arc_values \
  --test spsc_values \
  --test sequencer_spsc

cargo test -p glyphfmt
cargo test -p glyphlsp
```

## Deliberate release boundaries

The release does not expose safe `AtomicPtr<T>` or public memory-order
selection. `Arc<T>` provides atomic lifetime management and immutable shared
access; shared mutation uses atomic scalars, `Arc<Mutex<T>>`, or message
passing. General non-lexical lifetimes, detached borrowed tasks, MPSC/MPMC,
direct Glyph execution in a device callback, and user-defined `Send`/`Sync`
remain future work.

## Final sign-off

- [x] Working tree contains only reviewed release evidence changes.
- [x] Documentation describes the shipped behavior and all deliberate limits.
- [x] GLYPH-48 is ready to close with this page and commit-linked evidence.
- [x] GLYPH-32 is ready to close after every child is done.
