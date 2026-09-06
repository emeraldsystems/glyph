# Release Validation Matrix

## Purpose

The Rust integration test suite (`cargo test`) proves the compiler's internal
contracts hold, but it doesn't prove a user can actually install the
toolchain and get representative programs to build and run. This matrix
closes that gap: it builds `glyph-cli` once and then builds/runs a battery of
the example programs under `examples/` and `tests/fixtures/`, covering the
major language and stdlib surfaces (scalars, `Vec`/`Map`, JSON, file I/O,
process spawning, argv, multi-module imports, generics, closures, extern
`"C"` FFI, and the sequencer's concurrency/audio contracts).

It exists because GLYPH-11 asked for proof that "the full cargo suite is
green" also means real programs work — and because it caught a
release-blocking bug (below) that the unit-test suite does not exercise.

## How to run

```bash
just release-matrix
# or directly:
scripts/release-matrix.sh
```

The script is dependency-light (bash, cargo/rustc, curl; `timeout`/`gtimeout`
if present, else a `perl`-based fallback) and self-contained: it builds
`glyph-cli` via `cargo build -p glyph-cli`, locates the resulting binaries
through `cargo metadata`'s `target_directory` (so it works in a plain
checkout or a worktree with a redirected target dir), runs every row under a
per-row timeout (default 60s, override with `GLYPH_MATRIX_TIMEOUT`), and
prints a PASS/FAIL/SKIP table. All build output goes to a throwaway
`mktemp -d` directory or to each example's own `target/` (already
`**/target`-ignored) — nothing lands in the tracked tree.

Each row carries a severity:
- **BLOCKER** — must pass for a release to be considered clean. A failing
  BLOCKER row makes the script exit non-zero.
- **NON_GOAL** — informational only, always reported as SKIP with a reason;
  never affects the exit status.

**This script is the source of truth.** The table below records one run's
results; re-run the script on the actual release commit and update this
table (and the Failures section) with what it reports then.

## Run record

- **Date:** 2026-09-06
- **Commit:** `56e9cb9` (`Merge branch 'glyph-76-unsigned-cmp'`)
- **Host:** macOS (Darwin 25.5.0, arm64)
- **Command:** `scripts/release-matrix.sh`
- **Result:** 20 rows PASS, 1 row SKIP (documented non-goal), **1 BLOCKER row FAILED** (known: GLYPH-80) → script exited 1.

## Matrix

| # | Row | Command | Expected | Result |
|---|-----|---------|----------|--------|
| 1 | std_hello | `glyph-cli build examples/std_hello/hello.glyph --emit exe && ./hello` | exit 0, stdout contains `hello world` | PASS |
| 2 | puts_hello (extern C) | `glyph-cli build examples/puts_hello/hello.glyph --emit exe && ./hello` | exit 0, stdout `Hello World!` | PASS |
| 3 | extern_putchar.glyph fixture | `glyph-cli check ./extern_putchar.glyph` (copied to an isolated dir — see note) | exit 0, `check ok` | PASS |
| 4 | vector | `glyph run` in `examples/vector` | exit **2**, stdout contains `vector demo ran` and `status Some(30)` | PASS |
| 5 | map_basic | `glyph run` in `examples/map_basic` | exit 0, stdout `map has key 2` | PASS |
| 6 | json_types | `glyph run` in `examples/json_types` | exit 0, stdout contains `The JSON type system is working!` | PASS |
| 7 | json_parser | `glyph run` in `examples/json_parser` | exit 0, stdout contains `SUCCESS` | PASS |
| 8 | file_io | `glyph run` in `examples/file_io`, then verify `out.txt` | exit 0, `out.txt` contains `hello from glyph` | PASS |
| 9 | process_run | `glyph run` in `examples/process_run` | exit 0, stdout `process_run example` | PASS |
| 10 | argv, no args | `glyph run` in `examples/argv` | exit 0, stdout `argv example` | PASS (see note) |
| 11 | argv, with args | `glyph run -- one two three` in `examples/argv` | exit 0, stdout `argv example` | PASS |
| 12 | case_demo (path dependency) | `glyph run` in `examples/case_demo/my_app` | exit **2**, stdout contains `case demo` and `status Some(7)` | PASS |
| 13 | imports-demo | `glyph run` in `examples/imports-demo` | exit **2**, stdout contains `imports ok` and `status Some(33)` | PASS |
| 14 | generics_demo | `glyph run` in `examples/generics_demo` | exit 0 | PASS |
| 15 | closures | `glyph run` in `examples/closures` | exit 0 | PASS |
| 16 | bench_sum | `glyph run --release` in `examples/bench_sum` | exit 0 (time recorded, not gated) | PASS (~0.3s wall, dominated by process/JIT startup — the loop itself is 100 iterations) |
| 17 | glyph_tui/tui_demo | `glyph build` in `examples/glyph_tui/tui_demo` (build only, see note) | exit 0, `Built tui_demo` | PASS |
| 18 | apex (webserver) | `glyph build` in `examples/apex`, then `curl http://localhost:8080/` | exit 0, build succeeds, curl gets HTTP 200 | **FAIL — BLOCKER, known: GLYPH-80** |
| 19 | sequencer contracts.glyph | `glyph-cli build ./contracts.glyph --emit exe && ./contracts` (isolated dir) | exit 0 | PASS |
| 20 | sequencer spsc_proof.glyph | `glyph-cli build ./spsc_proof.glyph --emit exe && ./spsc_proof` (isolated dir) | exit 0, writes a valid WAV file | PASS |
| 21 | sequencer live engine / stdio server | — | — | **SKIP — NON_GOAL** |

Rows 3, 19, and 20 copy their fixture into an isolated scratch directory
before invoking `glyph-cli`. `glyph-cli check`/`build <file>` compiles every
`.glyph` file in the target file's *directory* as part of the same module
graph (see `AGENTS.md`'s "directory sweep" note), so running them in place
against `tests/fixtures/` would pull in unrelated fixtures — including ones
with real parse errors — and fail for reasons that have nothing to do with
the row under test.

## Failures

### BLOCKER — apex webserver build (known: GLYPH-80)

`examples/apex` fails to build with a move-checker false positive:

```
examples/apex/src/main.glyph:48: error: use of moved value `stream` of type `std::net::TcpStream`
examples/apex/src/main.glyph:48: note: Glyph uses single-owner move semantics and conservatively merges ownership across reachable control-flow paths
examples/apex/src/main.glyph:48: help: avoid branch-local moves before unconditional post-branch use, or reinitialize the value on every reachable branch
```

`stream` is moved into `serve_request(request, "www", stream)` on line 47 and
then used again on line 48 (`stream.close()`) — a genuine use-after-move by
Rust-style single-owner rules, but `serve_request` is written to *return*
ownership of the stream to the caller, so this is a real gap in what the
move checker can express (no "reborrow"/"move-out-and-back" signal), not a
bug in the example. Filed today as GLYPH-80 (per the integrator's brief);
this run confirms the diagnostic is exactly as described. `curl` was not
exercised since the binary never builds. **This must be fixed before
release** — it is the toolchain's own reference webserver example.

### NON_GOAL — sequencer live engine / stdio server

`examples/sequencer` no longer exists in this repository: the sequencer (a
16-voice synth/pattern engine, the largest Glyph program written) became the
audio engine of **GlyphAudio**, a separate application, and now lives in
that project's `engine/` directory (see `docs/book/src/sequencer.md`).
Exercising its live-playback mode (`glyph run` / `glyph run -- --server`)
requires audio hardware and a GlyphAudio checkout, neither of which is
available here — that is out of scope for this repository's release matrix
by design, not a gap in it.

What *is* still in this repo and still exercised (rows 19–20 above) are the
language-level contracts the sequencer depends on:
`tests/fixtures/sequencer/contracts.glyph` (SPSC channel + `spawn` +
`FnOnce`, streaming an `EngineCommand`/`EngineReport` protocol across two
threads with no `Mutex`) and `spsc_proof.glyph` (the same pattern, additionally
rendering samples and writing a 48kHz mono WAV file via `std/audio` — no live
device needed). Both build and run clean on this commit.

## Notable observations (not release blockers, but worth a follow-up ticket)

These didn't fail their row, or aren't part of the gated matrix, but are
real discrepancies worth tracking separately from GLYPH-11's scope (the
integrator's brief was explicit: fix stale example usage only, otherwise
just record repros — not fix compiler behavior).

### `glyph-cli run` segfaults on any program that calls into the stdlib runtime — likely NEW blocker

This is the most significant finding of this pass. The top-level
`README.md`'s own "Try It Out" section (lines 210–225) tells a new user to
run:

```bash
glyph-cli run examples/std_hello/hello.glyph
```

This **segfaults** (exit 139) with no output at all — not a diagnostic, a
native crash. Minimal repro:

```bash
cargo build -p glyph-cli
./target/debug/glyph-cli run examples/std_hello/hello.glyph   # crashes, exit 139
```

Isolating it: `glyph-cli run` on a program that only calls `extern "C"`
functions (`examples/puts_hello/hello.glyph`) or does pure arithmetic with no
stdlib calls (`examples/bench_sum/src/main.glyph`) **works fine** — only
`println` (i.e. anything that reaches the `std` stdlib's runtime C
functions) crashes. Root cause, from reading `crates/glyph-cli/src/main.rs`'s
`run()`: this command JIT-executes the module (`ctx.jit_execute_i32_with_symbols`)
and manually registers a small, explicit symbol table for the JIT
(`glyph_byte_at`, the `glyph_time_*` family, `glyph_process_run`, the
`glyph_term_*` family, plus whatever `thread_runtime::register_symbols`
adds) — but it does not register the `glyph_fmt_write_*` /
string-formatting runtime symbols that `println` lowers to, so the JIT
either can't resolve the call and jumps through a null/garbage pointer, or
resolves it to the wrong address. This is a different code path from the
`glyph` project tool's `run` (which does a real AOT build + subprocess
execute, and works correctly — that's what every row in this matrix uses)
and from `glyph-cli build --emit exe` (AOT, also fine, per rows 1–2, 19–20
above). Only the JIT `glyph-cli run` path is affected.

This did not become a matrix row (and does not gate this script) because no
row in the required matrix actually depends on `glyph-cli run`'s JIT path —
every row uses either `glyph run` (project tool, AOT) or `glyph-cli build
--emit exe` (AOT) — but it is exactly the second command in the README's own
quick-start, so a new user following the README hits a crash immediately.
Recommend filing a new ticket before release.

### `std/sys::argv` includes the program name, so `argv.len() == 0` is unreachable

`examples/argv/README.md` claims the program "exits with status 0 when
arguments exist, 1 otherwise," and `main.glyph` is written as
`if count == 0 { ret 1 }`. In `crates/glyph-backend/src/codegen/entry.rs`,
the generated `main` wrapper builds the `Vec<String>` for `argv` directly
from the process's raw `argc`/`argv` (`argv[0]` = the program path,
matching C's `argc`/`argv` convention) with no adjustment to drop the
program name. So `argv.len()` is always ≥ 1, and this row exits 0 with or
without extra arguments (row 10 above) — the README's "no arguments" branch
never fires in practice. This may be intentional (mirroring C) or may want
`argv` to behave like Rust's `std::env::args().skip(1)`; either way the
example and its README currently describe behavior the program doesn't
have. Not fixed here — no `[link]`-level API changed, so this isn't the
"stale example" case the integrator asked to fix.

### `glyph_tui`'s buffered/unbuffered output interleaves when stdout isn't a TTY

Building `examples/glyph_tui/tui_demo` and running the resulting binary with
stdout redirected to a file produces the entire spinner sequence (25 ticks
of `\r`+ANSI-clear+frame+"Loading..." bytes, ending in "Done!") *before* any
of the `puts()`-printed color/style demo text, even though the source calls
`puts("=== glyph_tui demo ===")` and all the color/style demonstrations
first, and only runs the spinner loop at the very end. This points to the
spinner's terminal writes (`glyph_term_write_str`/`glyph_term_flush`) and
`puts`'s stdio path using different buffering, which only becomes visible
once stdout isn't a TTY (line-buffered vs fully-buffered stdio, or a raw
`write(2)` vs `libc` `puts`). The demo still exits 0 either way, so this
didn't fail its row (build-only, per the matrix), and it's cosmetic for an
interactive terminal — but it's a real ordering bug worth a ticket if
`glyph_tui` output is ever captured/piped (logging, CI, `script(1)`, etc.).

### Pre-existing stale documentation not touched

Left as-is per the integrator's instruction to only fix stale example code,
not prose: `examples/json_parser/README.md` and `examples/json_types/README.md`
still describe `std/json/parser::parse` as "a temporary stub" that "always
returns `Null`" — it is not; row 7 above shows the real parser passing a
nested array/object/trailing-garbage-rejection check. `README.md`'s
"Documentation" section also points at a `docs/design/` directory that does
not exist in this checkout (design docs live in `docs/plan/`).

## Known compiler limitations hit or adjacent to this matrix

Per the integrator's brief, these are cited by ticket rather than re-filed:

- **GLYPH-80** (open, this run) — apex build failure, see Failures above.
- GLYPH-75, GLYPH-77, GLYPH-78, GLYPH-79 — open at this commit; none were
  triggered by any row in this matrix.
- GLYPH-72, GLYPH-73, GLYPH-74, GLYPH-76 — fixed and merged into this base
  (56e9cb9); nothing in this matrix regressed them.
