# Runtime thread-safety audit (GLYPH-44)

Status: implementation audit completed on 2026-08-22. This document is the
contract between the C runtime and the compiler's nominal `Send`/`Sync`
registry. "Thread-safe" below means free of C data races when the stated
ownership contract is followed; it does not promise deterministic ordering of
external I/O.

## Decision table

| Surface and exported APIs | Mutable process state | Runtime decision | Language contract |
| --- | --- | --- | --- |
| Time: `glyph_time_now`, `glyph_time_monotonic_ns`, `glyph_time_sleep_ms`, `glyph_time_sleep_us`, `glyph_time_sleep_until_ns`, `glyph_time_to_human_readable` | Only the human-readable function's 20-byte scratch buffer | The native runtime and CLI JIT shim both use thread-local buffers. A pointer is valid until the same thread formats another timestamp. Other time APIs use call-local state; absolute sleep recomputes its monotonic remainder after interruption. | The returned text view must be copied into an owned `String` before it can outlive the call boundary. No runtime handle policy is needed. |
| Network: `glyph_net_tcp_*`, `glyph_net_udp_*`, `glyph_net_bind`, `glyph_net_listen`, `glyph_net_accept`, `glyph_net_close`, `glyph_net_set_reuse_addr`, `glyph_net_get_last_error`, `glyph_net_local_port`, `glyph_net_tcp_send_file`, `glyph_file_size` | Compatibility error cache used by string-returning TCP/UDP receive functions | The cache is thread-local. A receive and its immediately following error query cannot be disturbed by another thread. Other state is kernel-owned or call-local; POSIX `errno` is already thread-local. | Socket values have unique close/receive state: `TcpStream`, `TcpListener`, and `UdpSocket` are `Send`, not `Sync`. Sharing requires a synchronization wrapper. |
| Terminal: `glyph_term_stdout`, `glyph_term_enter_ui_session`, `glyph_term_session_end`, `glyph_term_move_to`, `glyph_term_clear_line`, `glyph_term_write_str`, `glyph_term_flush`, `glyph_term_poll_event` | One process-wide active-session bit | An atomic compare/exchange admits exactly one session; session end is an atomic, idempotent release. The other current hooks are stateless. | `Terminal` and `UiSessionGuard` remain thread-affine and are neither `Send` nor `Sync`. The atomic prevents a C data race; it does not turn a process terminal into concurrently owned UI state. Only a successfully created guard may end the session. |
| Offline WAV: `glyph_audio_wav_open`, `glyph_audio_wav_write`, `glyph_audio_wav_close` | Sixteen-entry `FILE*` handle table | A runtime lock covers slot reservation, initialization, writes, header patching, close, and release. Independent writers may run on different threads; operations on one writer remain exclusive. | `WavWriter` is `Send`, not `Sync`. Its existing `&mut` methods provide the unique-operation requirement; shared access needs an explicit synchronization wrapper. |
| Live audio: `glyph_audio_out_open`, `glyph_audio_out_write`, `glyph_audio_out_close` | Four-entry AudioQueue handle table plus a per-device free-buffer pool | Slot reservation/release is serialized defensively. The existing per-device mutex protects the preallocated buffer pool. No global table lock, allocation, file I/O, or Glyph callback is added to the AudioQueue callback. | The live-audio surface is coordinated by one engine thread. `AudioOut` is neither `Send` nor `Sync`, and device open, write, and close remain engine-thread-affine. |
| Formatting/output: `glyph_fmt_write_*`, `glyph_print` | None; all conversion buffers are automatic storage | Formatting calls are reentrant. Each helper emits one `write` call, but output from different threads may be ordered arbitrarily. Glyph does not mutate the C locale, which the float conversion helpers rely on. | `Stdout` may be `Send + Sync`; callers needing message-level ordering must serialize complete messages. Borrowed string memory must remain valid for the call. |
| String conversion: `glyph_byte_at`, `glyph_string_from_*`, `glyph_f64_to_i32`, `glyph_string_char_at`, `glyph_string_index_of` | None; scratch buffers are automatic and returned strings are fresh allocations | Calls are reentrant. The caller exclusively owns and must eventually free each returned string. The same no-concurrent-locale-mutation condition applies to floating-point conversion. | Owned `String` values follow their normal `Send` policy; borrowed inputs must remain valid for the call. |
| Process: `glyph_process_run` | None; argv construction, child PID, and wait status are call-local | Concurrent `posix_spawnp`/`waitpid(child_pid)` calls are independent. The contract assumes no concurrent mutation of the process environment; Glyph exposes no environment mutation API. | No persistent process handle exists. The borrowed command and argument strings must remain alive for the synchronous call. |
| Compiler-generated file operations: `fopen`, `fread`, `fwrite`, `fseek`, `ftell`, `rewind`, `fclose` | libc-owned `FILE*`, stored in a Glyph `File` value | The runtime adds no global state. Separate streams are independent, but Glyph's seek/read/write/close sequence is stateful. | `File` is `Send`, not `Sync`: exclusive ownership may move to another thread, while concurrent operations or close require external synchronization. |
| Thread: `glyph_thread_spawn`, `glyph_thread_join`, `glyph_thread_detach` | Opaque per-thread state plus debug-only failure-injection state | Per-handle state is mutex-protected and reference-counted. Debug failure injection is mutex-protected. | `JoinHandle<T>` is `Send` when `T: Send`, never `Sync`, and join/detach consumes its unique handle. |

## Compiler registrations required by GLYPH-42

The runtime audit is not complete at the language boundary until the compiler
registers these canonical nominal identities. Structural inference from their
integer or pointer fields is not valid for opaque runtime resources.

| Canonical identity | `NominalThreadSafetyPolicy` |
| --- | --- |
| `std::io::Stdout` | `Audited { send: true, sync: true }` |
| `std::io::File` | `Audited { send: true, sync: false }` |
| `std::net::TcpStream` | `Audited { send: true, sync: false }` |
| `std::net::TcpListener` | `Audited { send: true, sync: false }` |
| `std::net::UdpSocket` | `Audited { send: true, sync: false }` |
| `std::term::Terminal` | `Audited { send: false, sync: false }` |
| `std::term::UiSessionGuard` | `Audited { send: false, sync: false }` |
| `std::audio::WavWriter` | `Audited { send: true, sync: false }` |
| `std::audio::AudioOut` | `Audited { send: false, sync: false }` |

`JoinHandle<T>` is a canonical application rather than a nominal leaf and must
use `CanonicalApplicationPolicy::JoinHandle`. The registry must default-deny
unrecognized runtime-handle types so a new handle cannot accidentally inherit
`Send + Sync` from an `i32` field.

## Real-time audio boundary

Glyph code never executes on the AudioQueue callback. The callback only returns
an already allocated queue buffer to the owning device's free list and signals
the existing condition variable. It does not allocate, format, access files,
acquire the new global handle-table lock, or invoke user destructors. This audit
does not claim that the callback is lock-free: the pre-existing per-device
mutex remains part of the AudioQueue bridge contract.

## Verification obligations

The focused runtime tests exercise cross-thread invariants without sleep or
timing assumptions: per-thread time buffers survive another thread's call,
per-thread network errors survive another thread's successful receive, a
simultaneous terminal-session race has exactly one winner, shared formatting
writes remain whole, synchronous process launches complete independently, and
independent libc file handles remain isolated. A barrier-synchronized WAV test
additionally holds all writers open together, verifies unique handles, writes
each stream, and validates its patched file size.

The CLI JIT shim has a matching barrier-synchronized regression: one thread
retains its formatted-time view while another formats a different timestamp,
proving the in-process symbol path follows the same thread-local contract as
the native runtime library.
