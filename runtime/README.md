# Glyph Runtime Library

This directory contains the C runtime library that provides low-level formatting and I/O support for Glyph programs.

## Callable ABI Boundary

Owned `FnOnce` values use a compiler-internal three-pointer representation:
an opaque environment pointer, an invoke-thunk pointer, and a drop-thunk
pointer. Capturing closures may allocate their environment; compiler-generated
invoke/drop thunks own its destruction. This is not a C ABI, and callable
values must not be passed directly through `extern "C"`.

Closure construction, invocation, and destruction may allocate, free, or run
arbitrary capture drop glue. None of those operations is guaranteed safe in a
hard real-time audio callback. The existing C device callback remains the
real-time boundary; construct and use Glyph closures on control/worker threads.

The authoritative representation, ownership, thread-safety, and audio-boundary
contract is documented in
[`docs/plan/CLOSURES_CONCURRENCY.md`](../docs/plan/CLOSURES_CONCURRENCY.md).

## Native Thread Result Ownership

`glyph_thread_spawn_result` stores a concrete return value in opaque runtime
state. Compiler-generated adapters normalize scalar and sret call ABIs; a
successful `glyph_thread_join_result` moves the bytes into caller-owned
uninitialized storage exactly once. Detach, including implicit handle-drop,
keeps the result runtime-owned and runs its compiler-generated drop thunk when
the worker and handle references are both gone. A failed OS join leaves the
handle and result intact for retry.

Normal language-level destruction is not guaranteed after process shutdown
begins. Programs that depend on result destructors for flushing or other
observable effects must join their workers before returning from `main`.
Detached workers must also finish before a JIT execution engine is destroyed,
because their generated entry and drop thunks belong to that engine.

## Mutex Runtime Boundary

`glyph_mutex_create`, `glyph_mutex_lock`, `glyph_mutex_try_lock`,
`glyph_mutex_unlock`, and `glyph_mutex_destroy` own an opaque, heap-stable
native mutex. The compiler separately owns the typed payload and emits its drop
glue, so `pthread_mutex_t` layout never becomes part of Glyph ABI.

`try_lock` returns `1` for ordinary contention, `0` for acquisition, and a
negative errno-style value for actual failures. Destroying a locked mutex
fails without consuming its pointer. Compiler-generated guard drop clears its
owned slot before unlocking, guaranteeing at most one unlock.

These calls are worker/control synchronization. They may block or enter the OS
and are prohibited on the hard real-time audio callback path. `Arc<Mutex<T>>`
has the same restriction; final release may also destroy `T` and free memory.

## Overview

The Glyph compiler generates LLVM IR that references external C functions for certain operations. This runtime library provides implementations of those functions.

## Files

- `glyph_fmt.c` - Formatting functions for converting Glyph values to text output

## Formatting Functions

### `glyph_fmt_write_i32`
```c
int glyph_fmt_write_i32(int fd, int32_t value)
```
Converts an i32 value to decimal ASCII representation and writes it to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: The i32 value to format
- **Returns:** Number of bytes written, or -1 on error

### `glyph_fmt_write_bool`
```c
int glyph_fmt_write_bool(int fd, bool value)
```
Writes "true" or "false" to the file descriptor based on the boolean value.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: The boolean value to format
- **Returns:** Number of bytes written (4 or 5), or -1 on error

### `glyph_fmt_write_u32`
```c
int glyph_fmt_write_u32(int fd, uint32_t value)
```
Formats an unsigned 32-bit integer as decimal ASCII and writes it to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: Unsigned 32-bit value to format
- **Returns:** Number of bytes written, or -1 on error

### `glyph_fmt_write_i64`
```c
int glyph_fmt_write_i64(int fd, int64_t value)
```
Formats a signed 64-bit integer as decimal ASCII and writes it to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: Signed 64-bit value to format
- **Returns:** Number of bytes written, or -1 on error

### `glyph_fmt_write_u64`
```c
int glyph_fmt_write_u64(int fd, uint64_t value)
```
Formats an unsigned 64-bit integer as decimal ASCII and writes it to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: Unsigned 64-bit value to format
- **Returns:** Number of bytes written, or -1 on error

### `glyph_fmt_write_str`
```c
int glyph_fmt_write_str(int fd, const char* str)
```
Writes a null-terminated string to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `str`: Pointer to null-terminated string
- **Returns:** Number of bytes written, or -1 on error

### `glyph_fmt_write_char`
```c
int glyph_fmt_write_char(int fd, uint32_t value)
```
Encodes a Unicode scalar value to UTF-8 and writes it to the file descriptor.
- **Parameters:**
  - `fd`: File descriptor (1 for stdout, 2 for stderr)
  - `value`: Unicode scalar value to format
- **Returns:** Number of bytes written, or -1 on error

## Building

To build the runtime library:

```bash
# Compile to object file
gcc -c -O2 -o glyph_fmt.o runtime/glyph_fmt.c

# Or create a static library
ar rcs libglyph_fmt.a glyph_fmt.o
```

## Linking

When compiling Glyph programs that use formatting (std::print, std::println, etc.), link against this runtime:

```bash
# Link with runtime
clang -o myprogram myprogram.o glyph_fmt.o

# Or link with static library
clang -o myprogram myprogram.o -L. -lglyph_fmt
```

## Usage in Glyph

These functions are automatically called by the stdlib formatting system:

```glyph
import std

fn main() -> i32 {
  std::print($"The answer is {42}");
  std::println($"Done: {true}");
  ret 0
}
```

The compiler lowers this to calls to:
- `std::io::fmt_write_i32(fd, 42)`
- `std::io::fmt_write_bool(fd, true)`

Which are extern declarations that link to `glyph_fmt_write_i32` and `glyph_fmt_write_bool`.

## Future Additions

Additional formatting functions to be added:
- `glyph_fmt_write_ptr` - Format pointer addresses
