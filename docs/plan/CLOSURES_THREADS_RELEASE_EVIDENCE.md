# Closures, Threads, and Atomicity Release Evidence (GLYPH-48)

**Status:** Working release-gate record; incomplete until every unchecked item
has a dated result and the referenced commit is immutable.

**Release commit:** `TBD`

**Platforms:** macOS `TBD`; Linux `TBD`

This page is the final index for GLYPH-32 evidence. It does not replace the
normative semantics in [CLOSURES_CONCURRENCY.md](CLOSURES_CONCURRENCY.md), the
runtime audit in [RUNTIME_THREAD_SAFETY_AUDIT.md](RUNTIME_THREAD_SAFETY_AUDIT.md),
or the detailed sanitizer transcript in
[CONCURRENCY_SANITIZER_EVIDENCE.md](CONCURRENCY_SANITIZER_EVIDENCE.md).

## Release gates

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-features --no-fail-fast -- --test-threads=4`
- [ ] `mdbook build docs/book`
- [ ] `just build && just install`
- [ ] Native and JIT acceptance tests pass on macOS.
- [ ] Native and JIT acceptance tests pass on Linux.
- [ ] AddressSanitizer runtime, Arc, mutex, SPSC, and scoped-thread suites pass.
- [ ] ThreadSanitizer runtime, Arc, mutex, SPSC, and scoped-thread suites pass,
      or a platform limitation and equivalent evidence are recorded.
- [ ] No ignored tests or unreviewed generated snapshots remain.
- [ ] Every GLYPH-32 child ticket links its final commit and focused evidence.

## Semantic acceptance

- [ ] Owned `FnOnce` invocation and uncalled-drop paths release captures once.
- [ ] Borrowed `Fn` repeats with shared capture loans and cannot mutate them.
- [ ] Borrowed `FnMut` repeats with exclusive capture loans and cannot alias.
- [ ] Borrowed callables cannot escape through return, storage, ownership, FFI,
      or an unscoped thread.
- [ ] Typed owned-thread join, detach, failure, and retry paths preserve exact
      result and capture ownership.
- [ ] Scoped children drain before callback locals on every control-flow exit;
      explicit and implicit joins release typed results exactly once.
- [ ] Structural `Send`/`Sync` diagnostics report the failing capture or field
      path; `Shared<T>` and raw pointers cannot be laundered through `Arc<T>`.
- [ ] Public atomics remain sequentially consistent; internal Arc and SPSC
      orderings match their acquire/release protocols in emitted LLVM IR.
- [ ] `Arc<T>` last-owner races, mutex guard cleanup, and SPSC saturation,
      wraparound, disconnect, and destructor stress tests pass.
- [ ] The sequencer golden render is deterministic and the live probe reports
      timing metrics without invoking Glyph code on the device callback.

## Focused command record

Record the date, commit, command, result count, and any platform-specific
filters for each run. Do not replace a failed result; append the successful
rerun and link the fix.

```text
Date / commit / platform:
Command:
Result:
Notes:
```

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

- [ ] Working tree contains only reviewed release changes.
- [ ] Documentation describes the shipped behavior and all deliberate limits.
- [ ] GLYPH-48 is closed with this page and immutable CI/sanitizer links.
- [ ] GLYPH-32 is closed only after every child is done.
