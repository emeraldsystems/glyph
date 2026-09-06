#!/usr/bin/env bash
#
# scripts/release-matrix.sh — GLYPH-11 release validation matrix.
#
# Builds glyph-cli once, then builds and runs a battery of representative
# example programs, recording PASS/FAIL/SKIP for each row and printing a
# summary table. This is the source of truth for docs/release/validation-matrix.md
# — re-run it on the release commit and update that doc with the results.
#
# Usage:
#   scripts/release-matrix.sh            # from anywhere; it locates the repo root
#
# Environment:
#   GLYPH_MATRIX_TIMEOUT   per-row timeout in seconds (default: 60)
#
# Exit status:
#   0  if every BLOCKER-severity row PASSed (or was SKIPped)
#   1  if any BLOCKER-severity row FAILed
#   NON_GOAL-severity rows are informational only and never affect the exit
#   status; they always come back as SKIP with a documented reason.
#
# Design notes:
#   - No `set -e`: every row must run even if an earlier one fails.
#   - Dependency-light: bash + the cargo/rustc toolchain + curl. Uses
#     `timeout`/`gtimeout` if present, else falls back to a perl-based
#     timeout wrapper (perl ships with macOS and virtually every Linux distro).
#   - Build outputs for single-file examples go to a throwaway temp dir, never
#     into the repo tree. Project examples (glyph.toml + `glyph build`/`run`)
#     write to their own `target/`, which is already git-ignored (`**/target`).

set -u

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT" || exit 1

ROW_TIMEOUT="${GLYPH_MATRIX_TIMEOUT:-60}"

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/glyph-release-matrix.XXXXXX")"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Timeout wrapper (portable)
# ---------------------------------------------------------------------------

TIMEOUT_BIN=""
for cand in timeout gtimeout; do
  if command -v "$cand" >/dev/null 2>&1; then
    TIMEOUT_BIN="$cand"
    break
  fi
done

timeout_run() {
  # timeout_run SECONDS cmd [args...]
  local secs="$1"
  shift
  if [[ -n "$TIMEOUT_BIN" ]]; then
    "$TIMEOUT_BIN" "$secs" "$@"
    return $?
  fi
  if command -v perl >/dev/null 2>&1; then
    perl -e '
      my $secs = shift(@ARGV);
      my @cmd = @ARGV;
      my $pid = fork();
      if (!defined $pid) { exit 126; }
      if ($pid == 0) {
        exec { $cmd[0] } @cmd or exit 127;
      }
      my $timed_out = 0;
      local $SIG{ALRM} = sub { $timed_out = 1; kill("TERM", $pid); };
      alarm($secs);
      waitpid($pid, 0);
      alarm(0);
      exit(124) if $timed_out;
      my $status = $?;
      exit(128 + ($status & 127)) if ($status & 127);
      exit($status >> 8);
    ' "$secs" "$@"
    return $?
  fi
  # No timeout mechanism available at all; run unbounded rather than fail.
  "$@"
}

# ---------------------------------------------------------------------------
# Result bookkeeping
# ---------------------------------------------------------------------------

ROW_NAME=()
ROW_SEVERITY=()   # BLOCKER | NON_GOAL
ROW_STATUS=()     # PASS | FAIL | SKIP
ROW_NOTE=()
ROW_SECS=()

record_row() {
  local name="$1" severity="$2" status="$3" note="$4" secs="${5:-}"
  ROW_NAME+=("$name")
  ROW_SEVERITY+=("$severity")
  ROW_STATUS+=("$status")
  ROW_NOTE+=("$note")
  ROW_SECS+=("$secs")
  printf '[%s] %-46s %s\n' "$status" "$name" "$note"
}

truncate_note() {
  # collapse newlines and cap length so the table stays readable
  local s="${1//$'\n'/ | }"
  if [[ ${#s} -gt 160 ]]; then
    s="${s:0:157}..."
  fi
  printf '%s' "$s"
}

# ---------------------------------------------------------------------------
# Build glyph-cli once, locate binaries
# ---------------------------------------------------------------------------

echo "=== Building glyph-cli (cargo build -p glyph-cli) ==="
if ! cargo build -p glyph-cli 2>&1 | tail -20; then
  echo "FATAL: cargo build -p glyph-cli failed; cannot run the matrix." >&2
  exit 1
fi

get_target_dir() {
  local td=""
  if command -v cargo >/dev/null 2>&1; then
    if command -v jq >/dev/null 2>&1; then
      td="$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq -r '.target_directory // empty')"
    else
      td="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
            | grep -o '"target_directory":"[^"]*"' | head -1 \
            | sed -E 's/"target_directory":"(.*)"/\1/')"
    fi
  fi
  if [[ -z "$td" ]]; then
    td="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  fi
  printf '%s\n' "$td"
}

TARGET_DIR="$(get_target_dir)"
GLYPH_CLI="$TARGET_DIR/debug/glyph-cli"
GLYPH="$TARGET_DIR/debug/glyph"

if [[ ! -x "$GLYPH_CLI" ]]; then
  echo "FATAL: expected glyph-cli binary at $GLYPH_CLI (not found or not executable)." >&2
  exit 1
fi
if [[ ! -x "$GLYPH" ]]; then
  echo "FATAL: expected glyph binary at $GLYPH (not found or not executable)." >&2
  exit 1
fi
echo "Using GLYPH_CLI=$GLYPH_CLI"
echo "Using GLYPH=$GLYPH"
echo

# ---------------------------------------------------------------------------
# Row helpers
# ---------------------------------------------------------------------------

# Run a `glyph` project command (build/run) inside a directory and capture
# combined output + exit code + wall time.
run_in_dir() {
  # run_in_dir DIR cmd [args...]   -> sets OUT, RC, SECS
  local dir="$1"
  shift
  local start end
  start="$(date +%s)"
  OUT="$(cd "$dir" && timeout_run "$ROW_TIMEOUT" "$@" 2>&1)"
  RC=$?
  end="$(date +%s)"
  SECS=$((end - start))
}

expect() {
  # expect NAME SEVERITY EXPECTED_RC SUBSTRING_OR_EMPTY
  # Uses $OUT/$RC/$SECS set by the most recent run_in_dir call.
  local name="$1" severity="$2" want_rc="$3" needle="$4"
  if [[ "$RC" -ne "$want_rc" ]]; then
    record_row "$name" "$severity" "FAIL" "exit=$RC want=$want_rc :: $(truncate_note "$OUT")" "$SECS"
    return
  fi
  if [[ -n "$needle" ]] && ! grep -qF -- "$needle" <<<"$OUT"; then
    record_row "$name" "$severity" "FAIL" "exit=$RC but missing expected substring '$needle' :: $(truncate_note "$OUT")" "$SECS"
    return
  fi
  record_row "$name" "$severity" "PASS" "exit=$RC" "$SECS"
}

# ===========================================================================
# Row 1: std_hello — single-file build (--emit exe) + run
# ===========================================================================
{
  workdir="$SCRATCH/std_hello"
  mkdir -p "$workdir"
  run_in_dir "$workdir" bash -c \
    "rm -f hello hello.o && '$GLYPH_CLI' build '$REPO_ROOT/examples/std_hello/hello.glyph' --emit exe && ./hello"
  expect "std_hello (glyph-cli build --emit exe + run)" BLOCKER 0 "hello world"
}

# ===========================================================================
# Row 2: puts_hello — extern "C" FFI, single-file build + run
# ===========================================================================
{
  workdir="$SCRATCH/puts_hello"
  mkdir -p "$workdir"
  run_in_dir "$workdir" bash -c \
    "rm -f hello hello.o && '$GLYPH_CLI' build '$REPO_ROOT/examples/puts_hello/hello.glyph' --emit exe && ./hello"
  expect "puts_hello (extern \"C\" FFI, build + run)" BLOCKER 0 "Hello World!"
}

# ===========================================================================
# Row 3: extern_putchar.glyph fixture — check only (no main; isolate from
# directory-sweep, see AGENTS/memory gotcha: glyph-cli compiles every .glyph
# file in the target file's directory).
# ===========================================================================
{
  workdir="$SCRATCH/extern_putchar"
  mkdir -p "$workdir"
  cp "$REPO_ROOT/tests/fixtures/extern_putchar.glyph" "$workdir/"
  run_in_dir "$workdir" "$GLYPH_CLI" check ./extern_putchar.glyph
  expect "extern_putchar.glyph fixture (check)" BLOCKER 0 "check ok"
}

# ===========================================================================
# Row 4: vector — glyph.toml project, Vec<T> push/pop/len/index
# NOTE: `glyph run` reports the child's exit status and itself exits 2 when
# that status is non-zero ("program exited with status Some(N)"); it does
# not propagate N as its own exit code. That is expected, current behavior.
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/vector" "$GLYPH" run
  expect "vector (glyph run)" BLOCKER 2 "vector demo ran"
  if ! grep -qF "status Some(30)" <<<"$OUT"; then
    echo "  note: vector — expected child exit 30, got: $(truncate_note "$OUT")"
  fi
}

# ===========================================================================
# Row 5: map_basic — glyph.toml project, Map<K,V> basics
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/map_basic" "$GLYPH" run
  expect "map_basic (glyph run)" BLOCKER 0 "map has key 2"
}

# ===========================================================================
# Row 6: json_types — std/json JsonValue construction
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/json_types" "$GLYPH" run
  expect "json_types (glyph run)" BLOCKER 0 "The JSON type system is working!"
}

# ===========================================================================
# Row 7: json_parser — std/json/parser::parse, nested array/object shapes
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/json_parser" "$GLYPH" run
  expect "json_parser (glyph run)" BLOCKER 0 "SUCCESS"
}

# ===========================================================================
# Row 8: file_io — File::create/write_string/close, then verify out.txt
# NOTE: out.txt is already tracked in git with this exact content, so a
# successful run is idempotent and leaves the tree clean.
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/file_io" "$GLYPH" run
  if [[ "$RC" -eq 0 ]] && grep -qF "hello from glyph" "$REPO_ROOT/examples/file_io/out.txt" 2>/dev/null; then
    record_row "file_io (glyph run, verifies out.txt)" BLOCKER PASS "exit=0, out.txt content ok" "$SECS"
  else
    record_row "file_io (glyph run, verifies out.txt)" BLOCKER FAIL "exit=$RC :: $(truncate_note "$OUT")" "$SECS"
  fi
}

# ===========================================================================
# Row 9: process_run — std/process::run("true", [])
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/process_run" "$GLYPH" run
  expect "process_run (glyph run)" BLOCKER 0 "process_run example"
}

# ===========================================================================
# Row 10/11: argv — std/sys::argv, with and without extra args
# NOTE: argv.len() counts argv[0] (the program path) like C's argc, so it is
# never 0 — the example's "no arguments -> exit 1" branch is unreachable in
# practice. Both invocations below are expected to exit 0. See the doc's
# "Notable observations" section.
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/argv" "$GLYPH" run
  expect "argv, no extra args (glyph run)" BLOCKER 0 "argv example"

  run_in_dir "$REPO_ROOT/examples/argv" "$GLYPH" run -- one two three
  expect "argv, with args (glyph run -- one two three)" BLOCKER 0 "argv example"
}

# ===========================================================================
# Row 12: case_demo — local path dependency (my_app -> my_lib)
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/case_demo/my_app" "$GLYPH" run
  expect "case_demo (glyph run, path dependency)" BLOCKER 2 "case demo"
  if ! grep -qF "status Some(7)" <<<"$OUT"; then
    echo "  note: case_demo — expected child exit 7, got: $(truncate_note "$OUT")"
  fi
}

# ===========================================================================
# Row 13: imports-demo — nested modules, `from` and `import` mixed
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/imports-demo" "$GLYPH" run
  expect "imports-demo (glyph run)" BLOCKER 2 "imports ok"
  if ! grep -qF "status Some(33)" <<<"$OUT"; then
    echo "  note: imports-demo — expected child exit 33, got: $(truncate_note "$OUT")"
  fi
}

# ===========================================================================
# Row 14: generics_demo — enums with generic-shaped variants
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/generics_demo" "$GLYPH" run
  expect "generics_demo (glyph run)" BLOCKER 0 ""
}

# ===========================================================================
# Row 15: closures — owned FnOnce closures
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/closures" "$GLYPH" run
  expect "closures (glyph run)" BLOCKER 0 ""
}

# ===========================================================================
# Row 16: bench_sum — release build + run, informational timing only
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/bench_sum" "$GLYPH" run --release
  expect "bench_sum (glyph run --release)" BLOCKER 0 ""
  echo "  note: bench_sum wall time: ${SECS}s (informational, not gated)"
}

# ===========================================================================
# Row 17: glyph_tui — build only (tui_demo depends on the glyph_tui lib).
# Not run: it interleaves buffered `puts` output with raw-fd spinner writes
# when stdout isn't a TTY, so its stdout order isn't a stable PASS signal.
# See the doc's "Notable observations" section.
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/glyph_tui/tui_demo" "$GLYPH" build
  expect "glyph_tui/tui_demo (glyph build, lib dependency)" BLOCKER 0 "Built tui_demo"
}

# ===========================================================================
# Row 18: apex — webserver build, then a live curl round-trip if it builds.
# GLYPH-80 (a move-checker error on `stream` at examples/apex/src/main.glyph:48,
# caused by main.glyph moving `stream` into serve_request() and then still
# calling stream.close() on it) is fixed: serve_request() now owns and closes
# the stream on every return path, and main.glyph no longer double-closes it.
# The `if [[ "$RC" -ne 0 ]]` branch below is kept as a guard in case of a
# future regression, rather than assuming the build always succeeds.
# ===========================================================================
{
  run_in_dir "$REPO_ROOT/examples/apex" "$GLYPH" build
  if [[ "$RC" -ne 0 ]]; then
    if grep -qF "use of moved value \`stream\`" <<<"$OUT" && grep -qF "main.glyph:48" <<<"$OUT"; then
      record_row "apex (glyph build)" BLOCKER FAIL "regression of GLYPH-80: use-of-moved-value on 'stream' at main.glyph:48 is back" "$SECS"
    else
      record_row "apex (glyph build)" BLOCKER FAIL "build failed with an UNEXPECTED diagnostic (not the former GLYPH-80 signature) :: $(truncate_note "$OUT")" "$SECS"
    fi
  else
    # Exercise the actual server. Its document root ("www") is a path
    # relative to the process's own working directory (see
    # examples/apex/README.md's "Build and run" section: it is documented to
    # be launched from inside examples/apex/) — so the binary must be started
    # with that directory as its cwd, not this script's. `exec` inside the
    # backgrounded subshell replaces the subshell with the apex process so
    # `$!` below is the apex PID itself, not the subshell wrapping it.
    APEX_DIR="$REPO_ROOT/examples/apex"
    APEX_BIN="$APEX_DIR/target/debug/apex"
    if [[ -x "$APEX_BIN" ]]; then
      (cd "$APEX_DIR" && exec "$APEX_BIN") >"$SCRATCH/apex.log" 2>&1 &
      apex_pid=$!
      sleep 1
      if curl_out="$(timeout_run 5 curl -s -o /dev/null -w '%{http_code}' http://localhost:8080/)"; then
        kill "$apex_pid" 2>/dev/null
        wait "$apex_pid" 2>/dev/null
        if [[ "$curl_out" == "200" ]]; then
          record_row "apex (glyph build + curl round-trip)" BLOCKER PASS "GLYPH-80 fixed; build ok, curl got HTTP 200" "$SECS"
        else
          record_row "apex (glyph build + curl round-trip)" BLOCKER FAIL "build ok but curl got HTTP $curl_out (want 200)" "$SECS"
        fi
      else
        kill "$apex_pid" 2>/dev/null
        wait "$apex_pid" 2>/dev/null
        record_row "apex (glyph build + curl round-trip)" BLOCKER FAIL "build ok but curl request failed/timed out" "$SECS"
      fi
    else
      record_row "apex (glyph build + curl round-trip)" BLOCKER FAIL "build reported success but binary not found at $APEX_BIN" "$SECS"
    fi
  fi
}

# ===========================================================================
# Row 19/20: sequencer language-level contracts — the real sequencer app
# moved to the separate GlyphAudio repo (engine/ there); these fixtures are
# what is left in THIS repo to prove the SPSC channel + spawn + audio-file
# contracts it depends on still compile and run. Neither needs audio hardware
# (spsc_proof writes a WAV file rather than opening a live device).
# ===========================================================================
{
  workdir="$SCRATCH/seq_contracts"
  mkdir -p "$workdir"
  cp "$REPO_ROOT/tests/fixtures/sequencer/contracts.glyph" "$workdir/"
  run_in_dir "$workdir" bash -c "'$GLYPH_CLI' build ./contracts.glyph --emit exe && ./contracts"
  expect "sequencer contracts.glyph (build+run, SPSC+thread)" BLOCKER 0 ""

  workdir="$SCRATCH/seq_spsc"
  mkdir -p "$workdir"
  cp "$REPO_ROOT/tests/fixtures/sequencer/spsc_proof.glyph" "$workdir/"
  run_in_dir "$workdir" bash -c "'$GLYPH_CLI' build ./spsc_proof.glyph --emit exe && ./spsc_proof"
  expect "sequencer spsc_proof.glyph (build+run, writes WAV)" BLOCKER 0 ""
}

# ===========================================================================
# Row 21: sequencer live engine (GlyphAudio) — documented non-goal
# ===========================================================================
record_row "sequencer live engine / stdio server (GlyphAudio)" NON_GOAL SKIP \
  "moved to the GlyphAudio repo (engine/); needs audio hardware; language contracts covered by the two rows above" ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo
echo "=== Release validation matrix summary ==="
printf '%-58s %-6s %-9s %-6s %s\n' "ROW" "STATUS" "SEVERITY" "SECS" "NOTE"
printf '%s\n' "--------------------------------------------------------------------------------------------------------------------------------------------"
blocker_failures=0
for i in "${!ROW_NAME[@]}"; do
  printf '%-58s %-6s %-9s %-6s %s\n' \
    "${ROW_NAME[$i]}" "${ROW_STATUS[$i]}" "${ROW_SEVERITY[$i]}" "${ROW_SECS[$i]}" "${ROW_NOTE[$i]}"
  if [[ "${ROW_SEVERITY[$i]}" == "BLOCKER" && "${ROW_STATUS[$i]}" == "FAIL" ]]; then
    blocker_failures=$((blocker_failures + 1))
  fi
done
echo

if [[ "$blocker_failures" -gt 0 ]]; then
  echo "RESULT: $blocker_failures BLOCKER row(s) failed. Release is NOT clean — see docs/release/validation-matrix.md." >&2
  exit 1
fi
echo "RESULT: all BLOCKER rows passed (NON_GOAL rows are informational only)."
exit 0
