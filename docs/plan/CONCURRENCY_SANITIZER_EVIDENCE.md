# Concurrency Sanitizer Evidence

This record covers the sanitizer acceptance evidence for typed thread results
(GLYPH-43), bounded SPSC channels (GLYPH-45), `Arc<T>` (GLYPH-50), and
`Mutex<T>` (GLYPH-51). The focused runs below used commit `870c27e` in a
detached worktree so unrelated in-progress language work could not change the
result while the checks were running.

## Environment

- Date: 2026-08-22
- Host: macOS 26.5.2 (25F84), Darwin 25.5.0, arm64
- Stable Rust: rustc 1.95.0 (LLVM 22.1.2)
- Nightly Rust: rustc 1.90.0-nightly 2025-07-31 (LLVM 20.1.8)
- Apple Clang: 21.0.0 (`clang-2100.1.1.101`)

`GLYPH_RUNTIME_SANITIZER=address|thread` is an opt-in build setting which
instruments every C runtime translation unit with Apple Clang and preserves
frame pointers. It is intentionally inactive in normal builds. The sanitizer
runtime is linked only into the target test executables.

## AddressSanitizer

Command (line-wrapped only for readability):

```sh
env GLYPH_RUNTIME_SANITIZER=address \
  ASAN_OPTIONS='halt_on_error=1' \
  CARGO_TARGET_DIR=/tmp/glyph-runtime-asan-target3 \
  cargo +nightly -Ztarget-applies-to-host -Zhost-config \
  --config 'target-applies-to-host=false' \
  --config 'target.aarch64-apple-darwin.rustflags=["-C","link-arg=-L/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/21/lib/darwin","-C","link-arg=-lclang_rt.asan_osx_dynamic","-C","link-arg=-Wl,-rpath,/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/21/lib/darwin"]' \
  test -p glyph-backend --all-features \
  --test thread_runtime --test arc_codegen \
  --test mutex_codegen --test mutex_runtime --test spsc_codegen -- \
  --skip aot_linker_resolves_and_executes_native_thread_runtime_symbol
```

Result: PASS.

- `arc_codegen`: 6 passed
- `mutex_codegen`: 4 passed
- `mutex_runtime`: 4 passed
- `spsc_codegen`: 5 passed
- `thread_runtime`: 10 passed, 1 deliberately filtered
- No AddressSanitizer report was emitted.

The filtered test invokes Glyph's production AOT linker from inside the test;
that nested linker command does not receive the sanitizer runtime arguments
and therefore cannot link an instrumented `libglyph_runtime.a`. The ordinary
unsanitized suite retains this AOT linker coverage.

Instrumentation was verified rather than inferred: `nm -u` on the generated
`glyph_thread.o` and `glyph_mutex.o` reported Apple ASan hooks including
`___asan_init`, `___asan_report_load8`, and `___asan_report_store8`.

Apple's ASan runtime on this host does not support LeakSanitizer. Setting
`ASAN_OPTIONS=detect_leaks=1` aborts before tests with:
`AddressSanitizer: detect_leaks is not supported on this platform.` Exact
drop counters and lifecycle assertions remain the leak evidence on macOS.

## ThreadSanitizer

Command:

```sh
env GLYPH_RUNTIME_SANITIZER=thread \
  TSAN_OPTIONS='halt_on_error=1' \
  CARGO_TARGET_DIR=/tmp/glyph-runtime-tsan-target \
  cargo +nightly -Ztarget-applies-to-host -Zhost-config \
  --config 'target-applies-to-host=false' \
  --config 'target.aarch64-apple-darwin.rustflags=["-C","link-arg=-L/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/21/lib/darwin","-C","link-arg=-lclang_rt.tsan_osx_dynamic","-C","link-arg=-Wl,-rpath,/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/21/lib/darwin"]' \
  test -p glyph-backend --all-features \
  --test thread_runtime --test arc_codegen \
  --test mutex_codegen --test mutex_runtime --test spsc_codegen -- \
  --skip aot_linker_resolves_and_executes_native_thread_runtime_symbol
```

Result: PASS.

- `arc_codegen`: 6 passed
- `mutex_codegen`: 4 passed
- `mutex_runtime`: 4 passed
- `spsc_codegen`: 5 passed
- `thread_runtime`: 10 passed, 1 deliberately filtered
- No ThreadSanitizer report was emitted.

`nm -u` on `glyph_thread.o` and `glyph_mutex.o` reported Apple TSan hooks
including `___tsan_init`, `___tsan_read8`, and `___tsan_write8`.

## Rust sanitizer runtime availability

Rust nightly advertises `-Zsanitizer`, but its bundled Darwin sanitizer
runtimes do not execute on this host. A minimal program containing only
`fn main() { println!("sanitizer probe"); }` produced these results:

```sh
rustc +nightly -Zsanitizer=address probe.rs -o /tmp/glyph_asan_probe
timeout 5 /tmp/glyph_asan_probe
# exit 124; startup spins in AsanInit/InitializeShadowMemory

rustc +nightly -Zsanitizer=thread probe.rs -o /tmp/glyph_tsan_probe
timeout 5 /tmp/glyph_tsan_probe
# exit 139; lldb identifies EXC_BAD_ACCESS in __tsan::SlotLock
```

Consequently, the supported sanitizer configuration on this machine is Apple
Clang instrumentation of the native Glyph runtime. Rust test code and LLVM
instructions produced dynamically by Glyph's JIT are not instrumented. In
particular, TSan cannot directly observe `Arc<T>` refcount or SPSC ring
instructions emitted by the JIT; those remain covered by atomic-IR assertions,
native-thread stress, last-owner/endpoint races, and exact destructor counters.
ASan still interposes the process allocator used by JIT code, but it cannot
diagnose an uninstrumented JIT load or store by itself.

## Ticket evidence

- GLYPH-43: the instrumented thread runtime passed typed join, retry, detach
  before/after completion, owned-result transfer, failure cleanup, and bounded
  lifecycle stress.
- GLYPH-45: SPSC empty/full/wrap/disconnect behavior, both final-endpoint drop
  orders, droppable payload cleanup, and high-iteration AOT contention passed
  in sanitizer-linked processes; dynamic ring instructions have the JIT
  limitation stated above.
- GLYPH-50: Arc clone/drop, nested payload, last-owner race, and native-thread
  stress passed in sanitizer-linked processes; dynamic Arc instructions have
  the JIT limitation stated above.
- GLYPH-51: the instrumented mutex runtime passed contention, visibility,
  try-lock, destroy-while-locked, and typed Arc/Mutex drop tests. A separate
  backend invariant test deliberately emits duplicate guard cleanup and proves
  the guard slot is nulled so the native mutex is unlocked exactly once.
